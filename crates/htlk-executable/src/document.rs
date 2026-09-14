//! Canonical document assembly and record/reference integrity.

use crate::digest::{Digest, ParseDigestError};
use crate::record_accounting::{EncodingLimitError, RecordAccounting};
use crate::{
    EnvelopeError, ExecutableEnvelope, ExecutionProfile, Expression, ExpressionKind as E,
    FunctionId, GraphRecordError, Identifier, JsonDocument, JsonError, Library, McpBinding,
    McpBindingKind, MetadataError, Operation, PolicyDocument, PolicyError, PortTable,
    PromptTemplate, Scope, ScopeContext, TypeError, ValueType, ValueTypeKind as T,
};
use htlk_cbor::{LimitKind, Limits, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// Authored canonical-document tables. Keys are assertions checked at assembly;
/// definition tables are not silently rekeyed or pruned.
#[derive(Clone, Debug, PartialEq)]
pub struct DocumentFields {
    /// Qualified snake_case root graph name.
    pub graph_id: String,
    /// Pinned implementation/policy profile.
    pub profile: ExecutionProfile,
    /// Required root scope digest.
    pub root_scope: Digest,
    /// Scope definitions, used in their actual ordinary/loop-body contexts.
    pub scopes: BTreeMap<Digest, Scope>,
    /// Reached local prompt templates.
    pub templates: BTreeMap<Digest, PromptTemplate>,
    /// Reached MCP bindings.
    pub bindings: BTreeMap<Digest, McpBinding>,
    /// Reached complete supplied library manifests keyed by implementation digest.
    pub libraries: BTreeMap<Digest, Library>,
    /// Exact absolute, fragment-free schema retrieval roots.
    pub schema_uris: BTreeMap<String, Digest>,
    /// Exact JCS documents keyed by raw SHA-256 digest.
    pub documents: BTreeMap<Digest, JsonDocument>,
}
impl DocumentFields {
    /// Starts authored fields with empty tables; insert the root scope and policy
    /// document before constructing CanonicalDocument.
    pub fn new(graph_id: String, profile: ExecutionProfile, root_scope: Digest) -> Self {
        Self {
            graph_id,
            profile,
            root_scope,
            scopes: BTreeMap::new(),
            templates: BTreeMap::new(),
            bindings: BTreeMap::new(),
            libraries: BTreeMap::new(),
            schema_uris: BTreeMap::new(),
            documents: BTreeMap::new(),
        }
    }
}

/// Immutable, canonically encoded document with table identities and known
/// references checked. This is NOT a fully verified/runnable graph: expression
/// typing/cycles, descriptor/protocol schemas, schema-resource closure, linked
/// implementation matching, and authorization require the shared verifier.
#[derive(Clone, Debug, PartialEq)]
pub struct CanonicalDocument {
    fields: Box<DocumentFields>,
    structure: crate::StructuralSummary,
}
impl CanonicalDocument {
    /// Checks complete fields, record keys, scope closure/roles/interfaces, known
    /// expression references, policy/JCS identities, and the policy's structural
    /// ceilings under codec limits.
    ///
    /// # Errors
    /// Returns structural, identity, reference, JSON/policy, or resource failures.
    pub fn new(fields: DocumentFields, limits: &Limits) -> Result<Self, DocumentError> {
        limits.validate()?;
        coarse_bounds(&fields, limits)?;
        let roles = scope_roles(
            fields.root_scope,
            fields.scopes.keys().copied().collect(),
            |id| {
                Ok(references(
                    fields
                        .scopes
                        .get(&id)
                        .ok_or(DocumentError::MissingRecord("scope"))?,
                ))
            },
        )?;
        let mut result = Self {
            fields: Box::new(fields),
            structure: crate::StructuralSummary::EMPTY,
        };
        result.build(&roles, limits)?; // Bound the whole document before semantic walks.
        result.structure = result.validate(&roles, limits)?;
        Ok(result)
    }
    /// Current canonical-document version.
    pub const fn ir_version(&self) -> &'static str {
        "0.1"
    }
    /// Borrows immutable tables and root/profile metadata.
    pub fn fields(&self) -> &DocumentFields {
        &self.fields
    }
    /// Derived scope depth and possible invocation count, within the pinned
    /// policy's structural ceilings. This summary is not serialized.
    pub const fn structural_summary(&self) -> crate::StructuralSummary {
        self.structure
    }
    /// Produces canonical document data under this call's limits.
    ///
    /// # Errors
    /// Returns resource or structural failures.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, DocumentError> {
        coarse_bounds(&self.fields, limits)?;
        let roles = scope_roles(
            self.fields.root_scope,
            self.fields.scopes.keys().copied().collect(),
            |id| {
                Ok(references(
                    self.fields
                        .scopes
                        .get(&id)
                        .ok_or(DocumentError::MissingRecord("scope"))?,
                ))
            },
        )?;
        self.build(&roles, limits)
    }
    /// Encodes exactly one canonical graph document.
    ///
    /// # Errors
    /// Returns codec/resource failures.
    pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, DocumentError> {
        Ok(htlk_cbor::encode(&self.to_value(limits)?, limits)?)
    }
    /// Wraps the canonical document bytes in the agreed executable envelope.
    ///
    /// # Errors
    /// Returns document/envelope resource failures; does not register the graph.
    pub fn envelope(&self, limits: &Limits) -> Result<ExecutableEnvelope, DocumentError> {
        Ok(ExecutableEnvelope::new(self.encode(limits)?, limits)?)
    }
    /// Decodes exactly one canonical document and checks its record integrity.
    ///
    /// # Errors
    /// Returns structural, reference, identity, or resource failures.
    pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, DocumentError> {
        Self::parse(&htlk_cbor::decode(bytes, limits)?, limits)
    }
    /// Checks a decoded canonical document, without normalizing noncanonical types.
    ///
    /// # Errors
    /// Returns structural, reference, identity, or resource failures.
    pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, DocumentError> {
        htlk_cbor::encode(value, limits)?;
        Self::parse(value, limits)
    }
    /// Checks the nested document of a separately checked envelope.
    ///
    /// # Errors
    /// Rejects empty, noncanonical, malformed, or integrity-invalid payloads.
    pub fn from_envelope(
        envelope: &ExecutableEnvelope,
        limits: &Limits,
    ) -> Result<Self, DocumentError> {
        Self::decode(envelope.payload(), limits)
    }

    fn build(&self, roles: &BTreeMap<Digest, u8>, limits: &Limits) -> Result<Value, DocumentError> {
        let f = &self.fields;
        let mut b = Builder::new(limits)?;
        b.text("ir_version", "0.1")?;
        b.text("graph_id", &f.graph_id)?;
        b.child("profile", f.profile.to_value(limits)?)?;
        b.text("root_scope", &f.root_scope.to_string())?;
        b.table(
            "scopes",
            f.scopes.len(),
            f.scopes.iter().map(|(id, scope)| {
                let role = *roles
                    .get(id)
                    .ok_or(DocumentError::UnreachableRecord("scope"))?;
                let v = scope.to_value(context(role), limits)?;
                if role == 3 {
                    scope.to_value(ScopeContext::LoopBody, limits)?;
                }
                Ok((id.to_string(), v))
            }),
        )?;
        b.table(
            "templates",
            f.templates.len(),
            f.templates
                .iter()
                .map(|(d, v)| Ok((d.to_string(), v.to_value(limits)?))),
        )?;
        b.table(
            "bindings",
            f.bindings.len(),
            f.bindings
                .iter()
                .map(|(d, v)| Ok((d.to_string(), v.to_value(limits)?))),
        )?;
        b.table(
            "libraries",
            f.libraries.len(),
            f.libraries
                .iter()
                .map(|(d, v)| Ok((d.to_string(), v.to_value(limits)?))),
        )?;
        b.table(
            "schema_uris",
            f.schema_uris.len(),
            f.schema_uris.iter().map(|(uri, d)| {
                RecordAccounting::new(limits)?.text(uri, 0)?;
                Ok((owned(uri)?, Value::Text(d.to_string())))
            }),
        )?;
        b.table(
            "documents",
            f.documents.len(),
            f.documents.iter().map(|(d, doc)| {
                // Document bytes are individually bounded before the fallible copy.
                check(
                    doc.as_bytes().len(),
                    limits.max_byte_string_bytes,
                    LimitKind::ByteStringBytes,
                )?;
                JsonDocument::decode(doc.as_bytes(), limits)?;
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(doc.as_bytes().len())
                    .map_err(allocation)?;
                bytes.extend_from_slice(doc.as_bytes());
                Ok((d.to_string(), Value::Bytes(bytes)))
            }),
        )?;
        b.finish()
    }

    fn parse(value: &Value, l: &Limits) -> Result<Self, DocumentError> {
        let m = closed(value)?;
        if text(field(m, "ir_version"))? != "0.1" {
            return Err(DocumentError::UnsupportedVersion);
        }
        let root = digest(field(m, "root_scope"))?;
        let raw_scopes = map(field(m, "scopes"))?;
        let keys = raw_scopes
            .iter()
            .map(|(k, _)| k.parse())
            .collect::<Result<BTreeSet<Digest>, _>>()?;
        let roles = scope_roles(root, keys, |d| {
            let raw = raw_scopes
                .get(&d.to_string())
                .ok_or(DocumentError::MissingRecord("scope"))?;
            let nodes = map(raw)?
                .get("nodes")
                .ok_or(DocumentError::MissingField("nodes"))?;
            let Value::Array(nodes) = nodes else {
                return Err(DocumentError::InvalidShape("scope nodes"));
            };
            let mut refs = Vec::new();
            for node in nodes {
                let op = map(node)?
                    .get("operation")
                    .ok_or(DocumentError::MissingField("operation"))?;
                match Operation::from_value(op, l)? {
                    Operation::Scope(d) => refs.push((d, 1)),
                    Operation::Loop { body, .. } => refs.push((body, 2)),
                    _ => (),
                }
            }
            Ok(refs)
        })?;
        let mut f = DocumentFields::new(
            text(field(m, "graph_id"))?.to_owned(),
            ExecutionProfile::from_value(field(m, "profile"), l)?,
            root,
        );
        for (key, value) in raw_scopes.iter() {
            let id: Digest = key.parse()?;
            let role = *roles
                .get(&id)
                .ok_or(DocumentError::UnreachableRecord("scope"))?;
            let scope = Scope::from_value(value, context(role), l)?;
            if role == 3 {
                Scope::from_value(value, ScopeContext::LoopBody, l)?;
            }
            f.scopes.insert(id, scope);
        }
        for (key, value) in map(field(m, "templates"))?.iter() {
            f.templates
                .insert(key.parse()?, PromptTemplate::from_value(value, l)?);
        }
        for (key, value) in map(field(m, "bindings"))?.iter() {
            f.bindings
                .insert(key.parse()?, McpBinding::from_value(value, l)?);
        }
        for (key, value) in map(field(m, "libraries"))?.iter() {
            f.libraries
                .insert(key.parse()?, Library::from_value(value, l)?);
        }
        for (uri, value) in map(field(m, "schema_uris"))?.iter() {
            f.schema_uris.insert(uri.to_owned(), digest(value)?);
        }
        for (key, value) in map(field(m, "documents"))?.iter() {
            let Value::Bytes(bytes) = value else {
                return Err(DocumentError::InvalidShape("document bytes"));
            };
            f.documents
                .insert(key.parse()?, JsonDocument::decode(bytes, l)?);
        }
        let mut result = Self {
            fields: Box::new(f),
            structure: crate::StructuralSummary::EMPTY,
        };
        result.structure = result.validate(&roles, l)?;
        Ok(result)
    }

    fn validate(
        &self,
        roles: &BTreeMap<Digest, u8>,
        l: &Limits,
    ) -> Result<crate::StructuralSummary, DocumentError> {
        let f = &self.fields;
        if f.graph_id
            .split('.')
            .any(|part| part.parse::<Identifier>().is_err())
        {
            return Err(DocumentError::InvalidGraphId);
        }
        for (d, scope) in &f.scopes {
            if scope.digest(context(roles[d]), l)? != *d {
                return Err(DocumentError::DigestMismatch("scope"));
            }
        }
        for (d, template) in &f.templates {
            if template.digest(l)? != *d {
                return Err(DocumentError::DigestMismatch("template"));
            }
        }
        for (d, binding) in &f.bindings {
            if binding.digest(l)? != *d {
                return Err(DocumentError::DigestMismatch("binding"));
            }
        }
        for (d, library) in &f.libraries {
            if library.implementation_digest() != *d {
                return Err(DocumentError::DigestMismatch("library key"));
            }
        }
        for (d, document) in &f.documents {
            if document.digest() != *d {
                return Err(DocumentError::DigestMismatch("JSON document"));
            }
        }
        let policy = f
            .documents
            .get(&f.profile.policy_document())
            .ok_or(DocumentError::MissingRecord("policy document"))?;
        let policy = PolicyDocument::decode(policy.as_bytes(), l)?;
        for (uri, d) in &f.schema_uris {
            let parsed = url::Url::parse(uri).map_err(|_| DocumentError::InvalidSchemaUri)?;
            if uri.bytes().any(|b| {
                !b.is_ascii()
                    || b.is_ascii_whitespace()
                    || b.is_ascii_control()
                    || matches!(
                        b,
                        b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
                    )
            }) || parsed.fragment().is_some()
                || uri.as_bytes().iter().enumerate().any(|(i, b)| {
                    *b == b'%'
                        && !uri
                            .as_bytes()
                            .get(i + 1..i + 3)
                            .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
                })
            {
                return Err(DocumentError::InvalidSchemaUri);
            }
            if !f.documents.contains_key(d) {
                return Err(DocumentError::MissingRecord("schema URI document"));
            }
        }
        let mut used = Used::default();
        let mut schema_roots = BTreeSet::new();
        for binding in f.bindings.values() {
            if let McpBindingKind::Tool {
                input_schema,
                output_schema,
                ..
            } = binding.kind()
            {
                schema_roots.extend([*input_schema, *output_schema]);
            }
        }
        for scope in f.scopes.values() {
            let s = scope.fields();
            for table in [&s.inputs, &s.outputs, &s.carried] {
                type_documents(table, f, &schema_roots)?;
            }
            expression(&s.preconditions, f, &mut used)?;
            expression(&s.postconditions, f, &mut used)?;
            for edge in &s.edges {
                expression(edge.guard(), f, &mut used)?;
            }
            for node in &s.nodes {
                let n = node.fields();
                type_documents(&n.inputs, f, &schema_roots)?;
                type_documents(&n.outputs, f, &schema_roots)?;
                for e in [&n.guard, &n.preconditions, &n.postconditions] {
                    expression(e, f, &mut used)?;
                }
                match &n.operation {
                    Operation::Eval(e) => expression(e, f, &mut used)?,
                    Operation::Mcp { binding, .. } => {
                        if !f.bindings.contains_key(binding) {
                            return Err(DocumentError::MissingRecord("binding"));
                        }
                        used.bindings.insert(*binding);
                        crate::binding_validation::tool_ports(n, &f.bindings[binding])?;
                    }
                    Operation::Scope(d) | Operation::Loop { body: d, .. } => {
                        let target = f
                            .scopes
                            .get(d)
                            .ok_or(DocumentError::MissingRecord("scope"))?
                            .fields();
                        if n.inputs != target.inputs || n.outputs != target.outputs {
                            return Err(DocumentError::ScopeInterfaceMismatch);
                        }
                        if let Operation::Loop {
                            initializers,
                            until,
                            ..
                        } = &n.operation
                        {
                            if initializers.len() != target.carried.len() {
                                return Err(DocumentError::ScopeInterfaceMismatch);
                            }
                            for (carried, input) in initializers {
                                let source = n
                                    .inputs
                                    .get(input.as_str())
                                    .ok_or(DocumentError::ScopeInterfaceMismatch)?;
                                if target.carried.get(carried.as_str()) != Some(source) {
                                    return Err(DocumentError::ScopeInterfaceMismatch);
                                }
                            }
                            expression(until, f, &mut used)?;
                        }
                    }
                    Operation::Wait { .. } => (),
                }
            }
        }
        exact(&used.bindings, f.bindings.keys(), "binding")?;
        exact(&used.templates, f.templates.keys(), "template")?;
        exact(&used.libraries, f.libraries.keys(), "library")?;
        for binding in f.bindings.values() {
            crate::binding_validation::descriptor(binding, f, l)?;
        }
        for lib in f.libraries.values() {
            for (_, sig) in lib.functions() {
                for port in sig
                    .parameters()
                    .iter()
                    .chain(std::iter::once(sig.returns()))
                {
                    type_refs(port.value_type(), f, &schema_roots)?;
                }
            }
        }
        crate::structure::analyze(f, policy.fields())
    }
}

fn context(role: u8) -> ScopeContext {
    if role & 1 != 0 {
        ScopeContext::Ordinary
    } else {
        ScopeContext::LoopBody
    }
}
fn references(scope: &Scope) -> Vec<(Digest, u8)> {
    scope
        .fields()
        .nodes
        .iter()
        .filter_map(|n| match &n.fields().operation {
            Operation::Scope(d) => Some((*d, 1)),
            Operation::Loop { body, .. } => Some((*body, 2)),
            _ => None,
        })
        .collect()
}
fn scope_roles(
    root: Digest,
    keys: BTreeSet<Digest>,
    mut refs: impl FnMut(Digest) -> Result<Vec<(Digest, u8)>, DocumentError>,
) -> Result<BTreeMap<Digest, u8>, DocumentError> {
    let mut roles = BTreeMap::new();
    let mut adjacency = BTreeMap::new();
    let mut work = vec![(root, 1)];
    while let Some((id, role)) = work.pop() {
        if !keys.contains(&id) {
            return Err(DocumentError::MissingRecord("scope"));
        }
        let known = roles.entry(id).or_insert(0);
        if *known & role != 0 {
            continue;
        }
        *known |= role;
        if let std::collections::btree_map::Entry::Vacant(e) = adjacency.entry(id) {
            let children = refs(id)?;
            work.extend(children.iter().copied());
            e.insert(children);
        }
    }
    if roles.len() != keys.len() {
        return Err(DocumentError::UnreachableRecord("scope"));
    }
    let mut degree: BTreeMap<_, usize> = keys.iter().map(|d| (*d, 0)).collect();
    for children in adjacency.values() {
        for (d, _) in children {
            *degree
                .get_mut(d)
                .ok_or(DocumentError::MissingRecord("scope"))? += 1;
        }
    }
    let mut ready: Vec<_> = degree
        .iter()
        .filter_map(|(d, n)| (*n == 0).then_some(*d))
        .collect();
    let mut count = 0;
    while let Some(d) = ready.pop() {
        count += 1;
        for (child, _) in &adjacency[&d] {
            let n = degree.get_mut(child).expect("key checked");
            *n -= 1;
            if *n == 0 {
                ready.push(*child);
            }
        }
    }
    if count != keys.len() {
        return Err(DocumentError::ScopeCycle);
    }
    Ok(roles)
}
fn coarse_bounds(f: &DocumentFields, l: &Limits) -> Result<(), DocumentError> {
    l.validate()?;
    let mut count = 0usize;
    for n in [
        f.scopes.len(),
        f.templates.len(),
        f.bindings.len(),
        f.libraries.len(),
        f.schema_uris.len(),
        f.documents.len(),
    ] {
        check(n, l.max_collection_entries, LimitKind::CollectionEntries)?;
        count = count.checked_add(n).ok_or(DocumentError::LimitExceeded {
            limit: LimitKind::TotalValues,
            maximum: l.max_total_values,
        })?;
    }
    for scope in f.scopes.values() {
        check(
            scope.fields().nodes.len(),
            l.max_collection_entries,
            LimitKind::CollectionEntries,
        )?;
        count =
            count
                .checked_add(scope.fields().nodes.len())
                .ok_or(DocumentError::LimitExceeded {
                    limit: LimitKind::TotalValues,
                    maximum: l.max_total_values,
                })?;
    }
    check(count, l.max_total_values, LimitKind::TotalValues)
}
#[derive(Default)]
struct Used {
    bindings: BTreeSet<Digest>,
    templates: BTreeSet<Digest>,
    libraries: BTreeSet<Digest>,
}
fn exact<'a>(
    used: &BTreeSet<Digest>,
    mut keys: impl Iterator<Item = &'a Digest>,
    kind: &'static str,
) -> Result<(), DocumentError> {
    if keys.any(|d| !used.contains(d)) {
        Err(DocumentError::UnreachableRecord(kind))
    } else {
        Ok(())
    }
}
fn expression(e: &Expression, f: &DocumentFields, used: &mut Used) -> Result<(), DocumentError> {
    match e.kind() {
        E::Call {
            function,
            arguments,
        } => {
            if let FunctionId::Library { library, name } = function {
                let lib = f
                    .libraries
                    .get(library)
                    .ok_or(DocumentError::MissingRecord("library"))?;
                let sig = lib
                    .function(name.as_str())
                    .ok_or(DocumentError::MissingRecord("function"))?;
                if sig.parameters().len() != arguments.len() {
                    return Err(DocumentError::FunctionArity);
                }
                used.libraries.insert(*library);
            }
            for e in arguments {
                expression(e, f, used)?;
            }
        }
        E::FunctionRef { library, name } => {
            let lib = f
                .libraries
                .get(library)
                .ok_or(DocumentError::MissingRecord("library"))?;
            if lib.function(name.as_str()).is_none() {
                return Err(DocumentError::MissingRecord("function"));
            }
            used.libraries.insert(*library);
        }
        E::Render {
            template,
            arguments,
        } => {
            let t = f
                .templates
                .get(template)
                .ok_or(DocumentError::MissingRecord("template"))?;
            if t.parameters().len() != arguments.len()
                || t.parameters()
                    .iter()
                    .zip(arguments)
                    .any(|((p, _), (a, _))| p != a)
            {
                return Err(DocumentError::TemplateArguments);
            }
            used.templates.insert(*template);
            for (_, e) in arguments {
                expression(e, f, used)?;
            }
        }
        E::List(v) => {
            for e in v {
                expression(e, f, used)?;
            }
        }
        E::Record(v) => {
            for (_, e) in v {
                expression(e, f, used)?;
            }
        }
        E::Get { value, .. } | E::Not(value) => expression(value, f, used)?,
        E::Binary { left, right, .. } => {
            expression(left, f, used)?;
            expression(right, f, used)?;
        }
        _ => (),
    }
    Ok(())
}
fn type_documents(
    t: &PortTable,
    f: &DocumentFields,
    roots: &BTreeSet<Digest>,
) -> Result<(), DocumentError> {
    for (_, p) in t.iter() {
        type_refs(p.value_type(), f, roots)?;
    }
    Ok(())
}
fn type_refs(
    t: &ValueType,
    f: &DocumentFields,
    roots: &BTreeSet<Digest>,
) -> Result<(), DocumentError> {
    match t.kind() {
        T::Schema(d) => {
            if !f.documents.contains_key(d) {
                return Err(DocumentError::MissingRecord("type schema document"));
            }
            if !roots.contains(d) {
                return Err(DocumentError::UnreachedSchemaType);
            }
        }
        T::List(t) | T::Map(t) => type_refs(t, f, roots)?,
        T::Record(fields) => {
            for (_, p) in fields {
                type_refs(p.value_type(), f, roots)?;
            }
        }
        T::Union(v) => {
            for t in v {
                type_refs(t, f, roots)?;
            }
        }
        T::Function {
            parameters,
            returns,
        } => {
            for p in parameters {
                type_refs(p.value_type(), f, roots)?;
            }
            type_refs(returns.value_type(), f, roots)?;
        }
        _ => (),
    }
    Ok(())
}

struct Builder<'a> {
    accounting: RecordAccounting<'a>,
    fields: Vec<(String, Value)>,
}
impl<'a> Builder<'a> {
    fn new(l: &'a Limits) -> Result<Self, DocumentError> {
        let mut a = RecordAccounting::new(l)?;
        a.collection(10, 0)?;
        Ok(Self {
            accounting: a,
            fields: Vec::new(),
        })
    }
    fn text(&mut self, key: &str, value: &str) -> Result<(), DocumentError> {
        self.accounting.text(key, 1)?;
        self.accounting.text(value, 1)?;
        self.push(key, Value::Text(owned(value)?))
    }
    fn push(&mut self, key: &str, v: Value) -> Result<(), DocumentError> {
        self.fields.try_reserve(1).map_err(allocation)?;
        self.fields.push((owned(key)?, v));
        Ok(())
    }
    fn child(&mut self, key: &str, v: Value) -> Result<(), DocumentError> {
        self.accounting.text(key, 1)?;
        self.accounting.value(&v, 1)?;
        self.push(key, v)
    }
    fn table(
        &mut self,
        key: &str,
        count: usize,
        entries: impl Iterator<Item = Result<(String, Value), DocumentError>>,
    ) -> Result<(), DocumentError> {
        self.accounting.text(key, 1)?;
        self.accounting.collection(count, 1)?;
        let mut pairs = Vec::new();
        for pair in entries {
            let (k, v) = pair?;
            self.accounting.text(&k, 2)?;
            self.accounting.value(&v, 2)?;
            pairs.try_reserve(1).map_err(allocation)?;
            pairs.push((k, v));
        }
        self.push(key, Value::Map(Map::try_from_entries(pairs)?))
    }
    fn finish(self) -> Result<Value, DocumentError> {
        Ok(Value::Map(Map::try_from_entries(self.fields)?))
    }
}
fn owned(s: &str) -> Result<String, DocumentError> {
    let mut v = String::new();
    v.try_reserve_exact(s.len()).map_err(allocation)?;
    v.push_str(s);
    Ok(v)
}
fn allocation(_: std::collections::TryReserveError) -> DocumentError {
    DocumentError::AllocationFailed
}
fn check(n: usize, maximum: usize, limit: LimitKind) -> Result<(), DocumentError> {
    if n > maximum {
        Err(DocumentError::LimitExceeded { limit, maximum })
    } else {
        Ok(())
    }
}
fn map(v: &Value) -> Result<&Map, DocumentError> {
    if let Value::Map(m) = v {
        Ok(m)
    } else {
        Err(DocumentError::InvalidShape("map"))
    }
}
fn text(v: &Value) -> Result<&str, DocumentError> {
    if let Value::Text(s) = v {
        Ok(s)
    } else {
        Err(DocumentError::InvalidShape("text"))
    }
}
fn digest(v: &Value) -> Result<Digest, DocumentError> {
    Ok(text(v)?.parse()?)
}
fn field<'a>(m: &'a Map, key: &str) -> &'a Value {
    m.get(key).expect("required fields checked")
}
fn closed(v: &Value) -> Result<&Map, DocumentError> {
    const FIELDS: [&str; 10] = [
        "bindings",
        "documents",
        "graph_id",
        "ir_version",
        "libraries",
        "profile",
        "root_scope",
        "schema_uris",
        "scopes",
        "templates",
    ];
    let m = map(v)?;
    if m.iter().any(|(k, _)| !FIELDS.contains(&k)) {
        return Err(DocumentError::UnknownField);
    }
    for field in FIELDS {
        if m.get(field).is_none() {
            return Err(DocumentError::MissingField(field));
        }
    }
    Ok(m)
}

/// Document assembly/integrity error with static, input-free diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocumentError {
    /// Codec error.
    Codec(htlk_cbor::Error),
    /// Envelope error.
    Envelope(EnvelopeError),
    /// Graph record error.
    Graph(GraphRecordError),
    /// Metadata error.
    Metadata(MetadataError),
    /// Expression/template error.
    Expression(crate::ExpressionError),
    /// Type error.
    Type(TypeError),
    /// Digest text error.
    Digest(ParseDigestError),
    /// External JSON error.
    Json(JsonError),
    /// Policy-schema error.
    Policy(PolicyError),
    /// Invalid native shape.
    InvalidShape(&'static str),
    /// Missing canonical field.
    MissingField(&'static str),
    /// Unknown canonical field.
    UnknownField,
    /// Unsupported canonical-document version.
    UnsupportedVersion,
    /// Graph name is not qualified snake_case.
    InvalidGraphId,
    /// Missing referenced record/function.
    MissingRecord(&'static str),
    /// A known definition table contains an unreachable record.
    UnreachableRecord(&'static str),
    /// Record/table digest mismatch.
    DigestMismatch(&'static str),
    /// Definition references contain a cycle (distinct from loop feedback).
    ScopeCycle,
    /// Scope/loop interface or initializer mismatch.
    ScopeInterfaceMismatch,
    /// Descriptor is not an object or its selection/schema field is invalid.
    InvalidDescriptor(&'static str),
    /// Tool schema identity differs from the exact descriptor subdocument.
    ToolSchemaMismatch(&'static str),
    /// Tool ports do not use the required exact input/output schema identities.
    McpInterfaceMismatch,
    /// A schema type does not identify a reached tool input/output schema root.
    UnreachedSchemaType,
    /// Pinned policy scope-depth or expanded-invocation ceiling exceeded.
    StructuralLimitExceeded {
        /// Exhausted structural resource, separate from codec limits.
        limit: crate::StructuralLimit,
        /// Exact positive policy ceiling.
        maximum: u64,
    },
    /// Library call argument count differs from its signature.
    FunctionArity,
    /// Render arguments do not exactly cover template parameters.
    TemplateArguments,
    /// Schema retrieval root is not absolute/fragment-free.
    InvalidSchemaUri,
    /// Resource ceiling exceeded.
    LimitExceeded {
        /// Exhausted resource.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// Allocation failed.
    AllocationFailed,
}
macro_rules! convert {
    ($ty:ty,$variant:ident) => {
        impl From<$ty> for DocumentError {
            fn from(e: $ty) -> Self {
                Self::$variant(e)
            }
        }
    };
}
convert!(htlk_cbor::Error, Codec);
convert!(EnvelopeError, Envelope);
convert!(GraphRecordError, Graph);
convert!(MetadataError, Metadata);
convert!(crate::ExpressionError, Expression);
convert!(TypeError, Type);
convert!(ParseDigestError, Digest);
convert!(JsonError, Json);
convert!(PolicyError, Policy);
impl From<EncodingLimitError> for DocumentError {
    fn from(e: EncodingLimitError) -> Self {
        Self::LimitExceeded {
            limit: e.limit,
            maximum: e.maximum,
        }
    }
}
impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "canonical document error: {self:?}")
    }
}
impl std::error::Error for DocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            Self::Envelope(e) => Some(e),
            Self::Graph(e) => Some(e),
            Self::Metadata(e) => Some(e),
            Self::Expression(e) => Some(e),
            Self::Type(e) => Some(e),
            Self::Digest(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Policy(e) => Some(e),
            _ => None,
        }
    }
}

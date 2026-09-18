//! Semantic document closure, interfaces and offline schema linkage.
use crate::{
    LinkageError as Error, NativeSchemaOptions, NativeSchemas, SchemaCatalog, StructuralSummary,
};
use htlk_executable::{
    cbor::{LimitKind, Limits},
    digest::Digest,
    *,
};
use std::collections::{BTreeMap, BTreeSet};

/// Focused offline document checks. Whole-document verification additionally
/// binds graph and expression plans to an immutable document through analyze_document.
pub trait DocumentAnalysis {
    /// Builds the exact reachable offline schema catalog.
    ///
    /// # Errors
    /// Returns missing/mismatched schema roots, closure or resource failures.
    fn schema_catalog(&self, limits: &Limits) -> Result<SchemaCatalog, Error>;
    /// Prepares native offline schemas and checks callable object roots.
    ///
    /// # Errors
    /// Returns catalog, native schema or resource failures.
    fn native_schemas(
        &self,
        options: NativeSchemaOptions,
        limits: &Limits,
    ) -> Result<NativeSchemas, Error>;
    /// Checks pinned MCP descriptor schemas and compound-selection consistency.
    ///
    /// # Errors
    /// Returns missing descriptors, protocol failures, conflicts or limits.
    fn validate_mcp_descriptors(&self, limits: &Limits) -> Result<(), Error>;
}
impl DocumentAnalysis for CanonicalDocument {
    fn schema_catalog(&self, limits: &Limits) -> Result<SchemaCatalog, Error> {
        self.to_value(limits)?;
        let f = self.fields();
        let mut roots = BTreeMap::new();
        for binding in f.bindings.values() {
            if let McpBindingKind::Tool {
                input_schema,
                output_schema,
                ..
            } = binding.kind()
            {
                for id in [input_schema, output_schema] {
                    if let std::collections::btree_map::Entry::Vacant(entry) = roots.entry(*id) {
                        let document = f
                            .documents
                            .get(id)
                            .ok_or(Error::MissingRecord("tool schema document"))?;
                        let base = crate::embedded_schema_base(document, limits)?;
                        if f.schema_uris.get(&base) != Some(id) {
                            return Err(Error::SchemaRootMismatch);
                        }
                        entry.insert(base);
                    }
                }
            }
        }
        let mut bytes = 0usize;
        for (uri, id) in &f.schema_uris {
            let document = f
                .documents
                .get(id)
                .ok_or(Error::MissingRecord("schema URI document"))?;
            for n in [uri.len(), document.as_bytes().len()] {
                bytes = bytes.checked_add(n).ok_or(Error::LimitExceeded {
                    limit: LimitKind::TotalPayloadBytes,
                    maximum: limits.max_total_payload_bytes,
                })?;
                check(
                    bytes,
                    limits.max_total_payload_bytes,
                    LimitKind::TotalPayloadBytes,
                )?;
            }
        }
        let mut documents = Vec::new();
        for (uri, id) in &f.schema_uris {
            let snapshot = JsonDocument::decode(f.documents[id].as_bytes(), limits)?;
            documents.try_reserve(1).map_err(allocation)?;
            documents.push((owned(uri)?, snapshot));
        }
        let catalog = SchemaCatalog::new(documents, limits)?;
        let mut root_uris = Vec::new();
        root_uris
            .try_reserve_exact(roots.len())
            .map_err(allocation)?;
        root_uris.extend(roots.values().map(String::as_str));
        let closure = catalog.reference_closure(&root_uris, limits)?;
        if closure.retrieval_uris().len() != f.schema_uris.len() {
            return Err(Error::UnreachableRecord("schema URI"));
        }
        let mut used_documents: BTreeSet<_> = f.schema_uris.values().copied().collect();
        used_documents.insert(f.profile.policy_document());
        used_documents.extend(f.bindings.values().map(McpBinding::descriptor));
        if f.documents.keys().any(|id| !used_documents.contains(id)) {
            return Err(Error::UnreachableRecord("JSON document"));
        }
        Ok(catalog)
    }
    fn native_schemas(
        &self,
        options: NativeSchemaOptions,
        limits: &Limits,
    ) -> Result<NativeSchemas, Error> {
        let catalog = self.schema_catalog(limits)?;
        let schemas = NativeSchemas::compile_diagnostic(&catalog, options, limits)
            .map_err(|e| Error::SchemaDiagnostic(Box::new(e)))?;
        for binding in self.fields().bindings.values() {
            if let McpBindingKind::Tool {
                input_schema,
                output_schema,
                ..
            } = binding.kind()
            {
                for id in [input_schema, output_schema] {
                    let document = self
                        .fields()
                        .documents
                        .get(id)
                        .ok_or(Error::MissingRecord("tool schema document"))?;
                    schemas.require_object_root(&crate::embedded_schema_base(document, limits)?)?;
                }
            }
        }
        Ok(schemas)
    }
    fn validate_mcp_descriptors(&self, limits: &Limits) -> Result<(), Error> {
        self.to_value(limits)?;
        let mut selections = BTreeMap::new();
        let mut checked = BTreeSet::new();
        for binding in self.fields().bindings.values() {
            use crate::McpDescriptorKind as K;
            let (kind, tag, selected) = match binding.kind() {
                McpBindingKind::Tool { name, .. } => (K::Tool, "tool", name),
                McpBindingKind::Resource { uri } => (K::Resource, "resource", uri),
                McpBindingKind::Template { uri_template } => {
                    (K::ResourceTemplate, "template", uri_template)
                }
                McpBindingKind::Prompt { name } => (K::Prompt, "prompt", name),
            };
            if checked.insert((tag, binding.descriptor())) {
                let descriptor = self
                    .fields()
                    .documents
                    .get(&binding.descriptor())
                    .ok_or(Error::MissingRecord("descriptor document"))?;
                crate::validate_mcp_descriptor(kind, descriptor, limits)?;
            }
            let key = (binding.server().digest(limits)?, tag, selected.as_str());
            if selections
                .insert(key, binding.descriptor())
                .is_some_and(|previous| previous != binding.descriptor())
            {
                return Err(Error::ConflictingMcpSelection);
            }
        }
        Ok(())
    }
}

pub(crate) fn verify(
    document: &CanonicalDocument,
    l: &Limits,
) -> Result<(PolicyDocument, StructuralSummary), Error> {
    document.to_value(l)?;
    let f = document.fields();
    let roles = scope_roles(f)?;
    for (id, scope) in &f.scopes {
        if roles[id] & 1 != 0 {
            crate::context::scope(scope, ScopeContext::Ordinary)?;
        }
        if roles[id] & 2 != 0 {
            crate::context::scope(scope, ScopeContext::LoopBody)?;
        }
    }
    let policy = f
        .documents
        .get(&f.profile.policy_document())
        .ok_or(Error::MissingRecord("policy document"))?;
    let policy = PolicyDocument::decode(policy.as_bytes(), l)?;
    for id in f.schema_uris.values() {
        if !f.documents.contains_key(id) {
            return Err(Error::MissingRecord("schema URI document"));
        }
    }
    for template in f.templates.values() {
        crate::context::template(template)?;
    }
    let mut used = Used::default();
    let mut schema_roots = BTreeSet::new();
    let mut template_variables = BTreeMap::new();
    for (id, binding) in &f.bindings {
        if let McpBindingKind::Template { uri_template } = binding.kind() {
            template_variables.insert(*id, uri_template_variables(uri_template, l)?);
        }
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
                    let b = f
                        .bindings
                        .get(binding)
                        .ok_or(Error::MissingRecord("binding"))?;
                    used.bindings.insert(*binding);
                    crate::binding_validation::ports(n, b, f)?;
                    if let Some(variables) = template_variables.get(binding) {
                        crate::binding_validation::template_ports(n, variables)?;
                    }
                }
                Operation::Scope(d) | Operation::Loop { body: d, .. } => {
                    let target = f
                        .scopes
                        .get(d)
                        .ok_or(Error::MissingRecord("scope"))?
                        .fields();
                    if n.inputs != target.inputs || n.outputs != target.outputs {
                        return Err(Error::ScopeInterfaceMismatch);
                    }
                    if let Operation::Loop {
                        initializers,
                        until,
                        ..
                    } = &n.operation
                    {
                        if initializers.len() != target.carried.len() {
                            return Err(Error::ScopeInterfaceMismatch);
                        }
                        for (carried, input) in initializers {
                            let source = n
                                .inputs
                                .get(input.as_str())
                                .ok_or(Error::ScopeInterfaceMismatch)?;
                            if target.carried.get(carried.as_str()) != Some(source) {
                                return Err(Error::ScopeInterfaceMismatch);
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
            crate::context::signature(sig)?;
            for port in sig
                .parameters()
                .iter()
                .chain(std::iter::once(sig.returns()))
            {
                type_refs(port.value_type(), f, &schema_roots)?;
            }
        }
    }
    let summary = crate::structure::analyze(f, policy.fields())?;
    Ok((policy, summary))
}

fn scope_roles(f: &DocumentFields) -> Result<BTreeMap<Digest, u8>, Error> {
    let mut roles = BTreeMap::new();
    let mut adjacency = BTreeMap::new();
    let mut work = vec![(f.root_scope, 1)];
    while let Some((id, role)) = work.pop() {
        let scope = f.scopes.get(&id).ok_or(Error::MissingRecord("scope"))?;
        let known = roles.entry(id).or_insert(0);
        if *known & role != 0 {
            continue;
        }
        *known |= role;
        if let std::collections::btree_map::Entry::Vacant(entry) = adjacency.entry(id) {
            let children: Vec<_> = scope
                .fields()
                .nodes
                .iter()
                .filter_map(|n| match &n.fields().operation {
                    Operation::Scope(d) => Some((*d, 1)),
                    Operation::Loop { body, .. } => Some((*body, 2)),
                    _ => None,
                })
                .collect();
            work.extend(children.iter().copied());
            entry.insert(children);
        }
    }
    if roles.len() != f.scopes.len() {
        return Err(Error::UnreachableRecord("scope"));
    }
    let mut degree: BTreeMap<_, usize> = f.scopes.keys().map(|d| (*d, 0)).collect();
    for children in adjacency.values() {
        for (d, _) in children {
            *degree.get_mut(d).ok_or(Error::MissingRecord("scope"))? += 1;
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
    if count != f.scopes.len() {
        return Err(Error::ScopeCycle);
    }
    Ok(roles)
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
) -> Result<(), Error> {
    if keys.any(|d| !used.contains(d)) {
        Err(Error::UnreachableRecord(kind))
    } else {
        Ok(())
    }
}
fn expression(e: &Expression, f: &DocumentFields, used: &mut Used) -> Result<(), Error> {
    use ExpressionKind as E;
    match e.kind() {
        E::Call {
            function,
            arguments,
        } => {
            if let FunctionId::Library { library, name } = function {
                let lib = f
                    .libraries
                    .get(library)
                    .ok_or(Error::MissingRecord("library"))?;
                let sig = lib
                    .function(name.as_str())
                    .ok_or(Error::MissingRecord("function"))?;
                if sig.parameters().len() != arguments.len() {
                    return Err(Error::FunctionArity);
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
                .ok_or(Error::MissingRecord("library"))?;
            if lib.function(name.as_str()).is_none() {
                return Err(Error::MissingRecord("function"));
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
                .ok_or(Error::MissingRecord("template"))?;
            if t.parameters().len() != arguments.len()
                || t.parameters()
                    .iter()
                    .zip(arguments)
                    .any(|((p, _), (a, _))| p != a)
            {
                return Err(Error::TemplateArguments);
            }
            used.templates.insert(*template);
            for (_, e) in arguments {
                expression(e, f, used)?;
            }
        }
        E::List(items) => {
            for e in items {
                expression(e, f, used)?;
            }
        }
        E::Record(fields) => {
            for (_, e) in fields {
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
    table: &PortTable,
    f: &DocumentFields,
    roots: &BTreeSet<Digest>,
) -> Result<(), Error> {
    for (_, p) in table.iter() {
        type_refs(p.value_type(), f, roots)?;
    }
    Ok(())
}
fn type_refs(t: &ValueType, f: &DocumentFields, roots: &BTreeSet<Digest>) -> Result<(), Error> {
    use ValueTypeKind as T;
    match t.kind() {
        T::Schema(d) => {
            if !f.documents.contains_key(d) {
                return Err(Error::MissingRecord("type schema document"));
            }
            if !roots.contains(d) {
                return Err(Error::UnreachedSchemaType);
            }
        }
        T::List(t) | T::Map(t) => type_refs(t, f, roots)?,
        T::Record(fields) => {
            for (_, p) in fields {
                type_refs(p.value_type(), f, roots)?;
            }
        }
        T::Union(types) => {
            for t in types {
                type_refs(t, f, roots)?;
            }
        }
        T::Function {
            parameters,
            returns,
        } => {
            for p in parameters.iter().chain(std::iter::once(returns.as_ref())) {
                type_refs(p.value_type(), f, roots)?;
            }
        }
        _ => (),
    }
    Ok(())
}
fn allocation(_: std::collections::TryReserveError) -> Error {
    Error::AllocationFailed
}
fn owned(s: &str) -> Result<String, Error> {
    let mut v = String::new();
    v.try_reserve_exact(s.len()).map_err(allocation)?;
    v.push_str(s);
    Ok(v)
}
fn check(n: usize, maximum: usize, limit: LimitKind) -> Result<(), Error> {
    if n > maximum {
        Err(Error::LimitExceeded { limit, maximum })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn symbolic_scope_dependencies_reject_cycles_without_recursion() {
        let limits = Limits::default();
        let id = Digest::from_bytes([7; 32]);
        let engine =
            EngineIdentity::new("engine".into(), "1".into(), "1".into(), id, &limits).unwrap();
        let profile =
            ExecutionProfile::new(id, engine.clone(), engine.clone(), engine, id, &limits).unwrap();
        let node = Node::new(
            NodeFields::new("again".parse().unwrap(), Operation::Scope(id)),
            ScopeContext::Ordinary,
            &limits,
        )
        .unwrap();
        let scope = Scope::new(
            ScopeFields {
                nodes: vec![node],
                ..ScopeFields::default()
            },
            ScopeContext::Ordinary,
            &limits,
        )
        .unwrap();
        let mut fields = DocumentFields::new("test.cycle".into(), profile, id);
        fields.scopes.insert(id, scope);
        assert_eq!(scope_roles(&fields), Err(Error::ScopeCycle));
    }
}

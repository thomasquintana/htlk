//! Bounded cross-document resource lookup over an explicitly supplied catalog.

use crate::digest::Digest;
use crate::{
    JsonDocument, JsonPointer, SchemaLocationError, SchemaResourceError as Error, SchemaResources,
};
use htlk_cbor::{LimitKind, Limits, Value};
use htlk_executable::cbor as htlk_cbor;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    retrieval: String,
    document: JsonDocument,
    index: SchemaResources,
}

/// Immutable offline schema catalog. All supplied documents are indexed; this
/// does not select a graph's reachable closure or canonical retrieval URI table.
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaCatalog {
    entries: Vec<Entry>,
    resources: BTreeMap<String, usize>,
}
impl SchemaCatalog {
    /// Indexes supplied snapshots and merges their resource claims. Identical
    /// duplicate retrieval entries coalesce; conflicting content fails. Equal
    /// resource-root copies may share a URI only when their base contexts match.
    ///
    /// # Errors
    /// Returns invalid schema/resource metadata, conflicting claims, or aggregate
    /// resource failures. Every document and derived child index is bounded too.
    pub fn new(mut documents: Vec<(String, JsonDocument)>, limits: &Limits) -> Result<Self, Error> {
        limits
            .validate()
            .map_err(crate::JsonError::from)
            .map_err(SchemaLocationError::from)?;
        check(
            documents.len(),
            limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        let mut accounting = Accounting { limits, bytes: 0 };
        for (uri, document) in &documents {
            check(
                uri.len(),
                limits.max_document_bytes,
                LimitKind::DocumentBytes,
            )?;
            check(
                document.as_bytes().len(),
                limits.max_document_bytes,
                LimitKind::DocumentBytes,
            )?;
            accounting.record(Some(uri))?;
            accounting.bytes(document.as_bytes().len())?;
        }
        documents.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        for pair in documents.windows(2) {
            if pair[0].0 == pair[1].0 && pair[0].1.as_bytes() != pair[1].1.as_bytes() {
                return Err(Error::RetrievalConflict);
            }
        }
        documents.dedup_by(|a, b| a.0 == b.0);
        let mut entries: Vec<Entry> = Vec::new();
        let mut resources: BTreeMap<String, usize> = BTreeMap::new();
        for (retrieval, document) in documents {
            let index = SchemaResources::new(&document, &retrieval, limits)?;
            index.stored_metadata(|text| accounting.record(text))?;
            for (uri, root) in index.resources() {
                if let Some(previous) = resources.get(uri) {
                    let previous = &entries[*previous];
                    let old_root = previous.index.resolve_absolute(uri, limits)?;
                    if previous.index.base_uri(old_root) != index.base_uri(root)
                        || old_root.resolve(&previous.document)? != root.resolve(&document)?
                    {
                        return Err(Error::ResourceConflict);
                    }
                    // Input order cannot select a different representative:
                    // equivalent resource copies use the first sorted retrieval.
                } else {
                    accounting.record(Some(uri))?;
                    let mut key = String::new();
                    key.try_reserve_exact(uri.len()).map_err(allocation)?;
                    key.push_str(uri);
                    resources.insert(key, entries.len());
                }
            }
            entries.try_reserve(1).map_err(allocation)?;
            entries.push(Entry {
                retrieval,
                document,
                index,
            });
        }
        Ok(Self { entries, resources })
    }
    /// Supplied retrieval URIs in exact string order, after identical duplicates coalesce.
    pub fn retrieval_uris(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.retrieval.as_str())
    }
    /// All derived resource URIs, including retrieval aliases and nested IDs.
    pub fn resource_uris(&self) -> impl Iterator<Item = &str> {
        self.resources.keys().map(String::as_str)
    }
    /// Borrows a complete supplied snapshot by exact retrieval URI.
    pub fn document(&self, retrieval_uri: &str) -> Option<&JsonDocument> {
        self.entry(retrieval_uri).map(|e| &e.document)
    }
    /// Resolves an initial reference from an explicit source document/location.
    /// No external retrieval or evaluation-time dynamic rebinding is performed.
    ///
    /// # Errors
    /// Returns missing source/target contexts, malformed references, non-schema
    /// targets, or query-construction limit failures. Limits do not rebuild the index.
    pub fn resolve(
        &self,
        source_retrieval: &str,
        from: &JsonPointer,
        reference: &str,
        limits: &Limits,
    ) -> Result<ResolvedSchema<'_>, Error> {
        let source = self.entry(source_retrieval).ok_or(Error::UnknownDocument)?;
        let uri = source.index.reference_uri(from, reference, limits)?;
        let resource_uri = uri.split_once('#').map_or(uri.as_str(), |(uri, _)| uri);
        let target = &self.entries[*self
            .resources
            .get(resource_uri)
            .ok_or(Error::MissingResource)?];
        let pointer = target.index.resolve_absolute(&uri, limits)?;
        Ok(ResolvedSchema {
            retrieval: &target.retrieval,
            document: &target.document,
            digest: target.index.document_digest(),
            pointer,
        })
    }
    /// Whether the initial named target was declared as a dynamic anchor.
    pub fn is_dynamic_anchor(&self, resource_uri: &str, name: &str) -> bool {
        self.resources
            .get(resource_uri)
            .is_some_and(|i| self.entries[*i].index.is_dynamic_anchor(resource_uri, name))
    }
    fn entry(&self, uri: &str) -> Option<&Entry> {
        self.entries
            .binary_search_by(|e| e.retrieval.as_str().cmp(uri))
            .ok()
            .map(|i| &self.entries[i])
    }
    /// Discovers a conservative document-level closure from exact retrieval roots.
    /// Every standard schema location in a reached document is scanned, including
    /// unused definitions; references in instance/annotation data are ignored.
    /// Schema cycles terminate without recursively expanding their occurrences.
    ///
    /// # Errors
    /// Returns malformed/missing references, unknown roots, non-schema targets,
    /// or bounded work/storage failures. Dynamic references record initial targets
    /// only. This does not enforce canonical executable closure or evaluate schemas.
    pub fn reference_closure<'a>(
        &'a self,
        roots: &[&str],
        limits: &Limits,
    ) -> Result<SchemaClosure<'a>, Error> {
        limits
            .validate()
            .map_err(crate::JsonError::from)
            .map_err(SchemaLocationError::from)?;
        check(
            roots.len(),
            limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        let mut work = ClosureWork {
            accounting: Accounting { limits, bytes: 0 },
            reached: BTreeSet::new(),
            pending: BTreeSet::new(),
            resources: BTreeMap::new(),
        };
        for root in roots {
            let i = self
                .entries
                .binary_search_by(|e| e.retrieval.as_str().cmp(root))
                .map_err(|_| Error::UnknownDocument)?;
            work.enqueue(i, &self.entries[i])?;
        }
        let mut pending = Vec::new();
        while let Some(i) = work.pending.pop_first() {
            let entry = &self.entries[i];
            for pointer in entry.index.pointers() {
                work.accounting.record(None)?;
                account_pointer(&mut work.accounting, pointer)?;
                let Value::Map(m) = pointer.resolve(&entry.document)? else {
                    continue;
                };
                for kind in [SchemaReferenceKind::DynamicRef, SchemaReferenceKind::Ref] {
                    let Some(value) = m.get(kind.as_str()) else {
                        continue;
                    };
                    let Value::Text(reference) = value else {
                        return Err(Error::InvalidReference(kind.as_str()));
                    };
                    work.accounting.record(None)?;
                    let uri = entry.index.reference_uri(pointer, reference, limits)?;
                    work.accounting.bytes(uri.len())?;
                    let resource = uri.split_once('#').map_or(uri.as_str(), |(r, _)| r);
                    if !work.resources.contains_key(resource)
                        && let Ok(target) = self
                            .entries
                            .binary_search_by(|e| e.retrieval.as_str().cmp(resource))
                    {
                        work.enqueue(target, &self.entries[target])?;
                    }
                    // A referenced resource can become known through a later
                    // reached document. Check missing targets after discovery.
                    pending.try_reserve(1).map_err(allocation)?;
                    pending.push((i, pointer, kind, reference.as_str(), uri));
                }
            }
        }
        let mut references = Vec::new();
        references
            .try_reserve_exact(pending.len())
            .map_err(allocation)?;
        for (source, pointer, kind, reference, uri) in pending {
            let resource = uri.split_once('#').map_or(uri.as_str(), |(r, _)| r);
            let target = if self.entries[source].index.has_resource(resource) {
                source
            } else {
                *work.resources.get(resource).ok_or(Error::MissingResource)?
            };
            let entry = &self.entries[target];
            let fragment = uri.split_once('#').map_or("", |(_, fragment)| fragment);
            if fragment.starts_with('/')
                || fragment
                    .get(..3)
                    .is_some_and(|s| s.eq_ignore_ascii_case("%2f"))
            {
                // Joining a short reference to a long resource-root pointer must
                // not amplify work beyond the closure's aggregate allowance.
                let root = entry.index.resolve_absolute(resource, limits)?;
                account_pointer(&mut work.accounting, root)?;
            }
            let target_pointer = entry.index.resolve_absolute(&uri, limits)?;
            references.push(SchemaReference {
                source_retrieval: &self.entries[source].retrieval,
                source_pointer: pointer,
                kind,
                reference,
                target: ResolvedSchema {
                    retrieval: &entry.retrieval,
                    document: &entry.document,
                    digest: entry.index.document_digest(),
                    pointer: target_pointer,
                },
            });
        }
        references.sort_unstable_by(|a, b| {
            a.source_retrieval
                .cmp(b.source_retrieval)
                .then_with(|| a.source_pointer.tokens().cmp(b.source_pointer.tokens()))
                .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
        });
        let mut retrievals = Vec::new();
        retrievals
            .try_reserve_exact(work.reached.len())
            .map_err(allocation)?;
        retrievals.extend(
            work.reached
                .into_iter()
                .map(|i| self.entries[i].retrieval.as_str()),
        );
        Ok(SchemaClosure {
            retrievals,
            references,
        })
    }
}

struct ClosureWork<'a, 'l> {
    accounting: Accounting<'l>,
    reached: BTreeSet<usize>,
    pending: BTreeSet<usize>,
    resources: BTreeMap<&'a str, usize>,
}
fn account_pointer(accounting: &mut Accounting<'_>, pointer: &JsonPointer) -> Result<(), Error> {
    for token in pointer.tokens() {
        accounting.bytes(token.len())?;
        accounting.bytes(1)?;
        accounting.bytes(token.bytes().filter(|b| matches!(b, b'~' | b'/')).count())?;
    }
    Ok(())
}
impl<'a> ClosureWork<'a, '_> {
    fn enqueue(&mut self, i: usize, entry: &'a Entry) -> Result<(), Error> {
        if self.reached.contains(&i) {
            return Ok(());
        }
        let l = self.accounting.limits;
        check(
            entry.retrieval.len(),
            l.max_document_bytes,
            LimitKind::DocumentBytes,
        )?;
        self.accounting.record(Some(&entry.retrieval))?;
        self.accounting.bytes(entry.document.as_bytes().len())?;
        JsonDocument::decode(entry.document.as_bytes(), l).map_err(SchemaLocationError::from)?;
        for (uri, _) in entry.index.resources() {
            check(uri.len(), l.max_document_bytes, LimitKind::DocumentBytes)?;
            if let Some(previous) = self.resources.get_mut(uri) {
                *previous = (*previous).min(i);
            } else {
                self.accounting.record(Some(uri))?;
                self.resources.insert(uri, i);
            }
        }
        self.reached.insert(i);
        self.pending.insert(i);
        Ok(())
    }
}

/// Reference keyword category; a dynamic reference still has an initial target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaReferenceKind {
    /// Standard static reference.
    Ref,
    /// Dynamic reference, before evaluation-time rebinding.
    DynamicRef,
}
impl SchemaReferenceKind {
    /// Exact JSON Schema keyword spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ref => "$ref",
            Self::DynamicRef => "$dynamicRef",
        }
    }
}
/// Borrowed reference metadata retaining exact source and initial target context.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SchemaReference<'a> {
    source_retrieval: &'a str,
    source_pointer: &'a JsonPointer,
    kind: SchemaReferenceKind,
    reference: &'a str,
    target: ResolvedSchema<'a>,
}
impl<'a> SchemaReference<'a> {
    /// Exact retrieval context containing the reference.
    pub const fn source_retrieval_uri(self) -> &'a str {
        self.source_retrieval
    }
    /// Schema object carrying the reference keyword.
    pub const fn source_pointer(self) -> &'a JsonPointer {
        self.source_pointer
    }
    /// Static or dynamic reference keyword.
    pub const fn kind(self) -> SchemaReferenceKind {
        self.kind
    }
    /// Exact stored URI reference string.
    pub const fn reference(self) -> &'a str {
        self.reference
    }
    /// Initial target; no dynamic-scope rebinding has been performed.
    pub const fn target(self) -> ResolvedSchema<'a> {
        self.target
    }
}
/// Conservative closure over complete reached documents, not an executable
/// acceptance result or a minimal validation-path subgraph.
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaClosure<'a> {
    retrievals: Vec<&'a str>,
    references: Vec<SchemaReference<'a>>,
}
impl<'a> SchemaClosure<'a> {
    /// Reached retrieval contexts in exact URI string order.
    pub fn retrieval_uris(&self) -> &[&'a str] {
        &self.retrievals
    }
    /// References sorted by source retrieval, decoded pointer tokens, and keyword.
    pub fn references(&self) -> &[SchemaReference<'a>] {
        &self.references
    }
}

/// Borrowed initial target retaining retrieval context, complete-document identity,
/// and the actual schema pointer. It is not an independently extracted schema.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedSchema<'a> {
    retrieval: &'a str,
    document: &'a JsonDocument,
    digest: Digest,
    pointer: &'a JsonPointer,
}
impl<'a> ResolvedSchema<'a> {
    /// Retrieval context of the deterministic representative document.
    pub const fn retrieval_uri(self) -> &'a str {
        self.retrieval
    }
    /// Digest of the complete representative JSON document.
    pub const fn document_digest(self) -> Digest {
        self.digest
    }
    /// Location within the complete representative document.
    pub const fn pointer(self) -> &'a JsonPointer {
        self.pointer
    }
    /// Borrows the complete snapshot, preserving the target's context.
    pub const fn document(self) -> &'a JsonDocument {
        self.document
    }
    /// Borrows the object/Boolean schema at the resolved target.
    pub fn value(self) -> &'a Value {
        self.pointer
            .resolve(self.document)
            .expect("indexed schema location")
    }
}

struct Accounting<'a> {
    limits: &'a Limits,
    bytes: usize,
}
impl Accounting<'_> {
    fn record(&mut self, text: Option<&str>) -> Result<(), Error> {
        // One byte per record marker, plus any associated text.
        self.bytes(1)?;
        self.bytes(text.map_or(0, str::len))
    }
    fn bytes(&mut self, count: usize) -> Result<(), Error> {
        self.bytes = self.bytes.checked_add(count).ok_or(Error::LimitExceeded {
            limit: LimitKind::DocumentBytes,
            maximum: self.limits.max_document_bytes,
        })?;
        check(
            self.bytes,
            self.limits.max_document_bytes,
            LimitKind::DocumentBytes,
        )
    }
}
fn check(n: usize, maximum: usize, limit: LimitKind) -> Result<(), Error> {
    if n > maximum {
        Err(Error::LimitExceeded { limit, maximum })
    } else {
        Ok(())
    }
}
fn allocation(_: std::collections::TryReserveError) -> Error {
    Error::AllocationFailed
}

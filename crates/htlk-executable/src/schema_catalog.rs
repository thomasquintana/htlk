//! Bounded cross-document resource lookup over an explicitly supplied catalog.

use crate::digest::Digest;
use crate::{
    JsonDocument, JsonPointer, SchemaLocationError, SchemaResourceError as Error, SchemaResources,
};
use htlk_cbor::{LimitKind, Limits, Value};
use std::collections::BTreeMap;

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
            limits.max_collection_entries,
            LimitKind::CollectionEntries,
        )?;
        let mut accounting = Accounting {
            limits,
            values: 0,
            payload: 0,
        };
        for (uri, document) in &documents {
            check(uri.len(), limits.max_text_bytes, LimitKind::TextBytes)?;
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
                    check(
                        resources.len() + 1,
                        limits.max_collection_entries,
                        LimitKind::CollectionEntries,
                    )?;
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
    values: usize,
    payload: usize,
}
impl Accounting<'_> {
    fn record(&mut self, text: Option<&str>) -> Result<(), Error> {
        self.values = self.values.checked_add(1).ok_or(Error::LimitExceeded {
            limit: LimitKind::TotalValues,
            maximum: self.limits.max_total_values,
        })?;
        check(
            self.values,
            self.limits.max_total_values,
            LimitKind::TotalValues,
        )?;
        if let Some(text) = text {
            self.bytes(text.len())?;
        }
        Ok(())
    }
    fn bytes(&mut self, count: usize) -> Result<(), Error> {
        self.payload = self
            .payload
            .checked_add(count)
            .ok_or(Error::LimitExceeded {
                limit: LimitKind::TotalPayloadBytes,
                maximum: self.limits.max_total_payload_bytes,
            })?;
        check(
            self.payload,
            self.limits.max_total_payload_bytes,
            LimitKind::TotalPayloadBytes,
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

//! Offline resource/anchor indexing within one supplied schema document.

use crate::digest::Digest;

/// Selects the prescribed retrieval base for an embedded input/output schema.
/// An absolute root `$id` is used exactly (removing an empty fragment); otherwise
/// the base is `urn:htlk:schema:` plus the raw JCS digest's 64 lowercase hex digits.
/// No catalog alias participates in this choice and the JSON is not rewritten.
///
/// # Errors
/// Returns invalid JSON/URI metadata or resource-limit errors. Relative root IDs
/// select the synthetic base here; indexing then applies normal ID resolution and
/// rejects non-fragment relative IDs against that opaque base.
pub fn embedded_schema_base(
    document: &JsonDocument,
    limits: &Limits,
) -> Result<String, SchemaResourceError> {
    JsonDocument::decode(document.as_bytes(), limits).map_err(SchemaLocationError::from)?;
    let id = match document.value() {
        Value::Map(m) => m.get("$id"),
        Value::Bool(_) => None,
        _ => return Err(SchemaLocationError::InvalidSchema.into()),
    };
    if let Some(id) = id {
        let Value::Text(id) = id else {
            return Err(SchemaResourceError::InvalidKeyword("$id"));
        };
        text_limit(id, limits)?;
        let parsed =
            UriReferenceStr::new(id).map_err(|_| SchemaResourceError::InvalidKeyword("$id"))?;
        if parsed.scheme_str().is_some() {
            let base = id.strip_suffix('#').unwrap_or(id);
            UriAbsoluteStr::new(base).map_err(|_| SchemaResourceError::InvalidKeyword("$id"))?;
            return owned(base);
        }
    }
    let digest = document.digest().to_string();
    bounded(format_args!("urn:htlk:schema:{}", &digest[7..]), limits)
}
use crate::{JsonDocument, JsonPointer, JsonPointerError, SchemaLocationError, SchemaLocations};
use htlk_cbor::{LimitKind, Limits, Value};
use htlk_executable::cbor as htlk_cbor;
use iri_string::types::{UriAbsoluteStr, UriReferenceStr};
use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
};

/// Derived resource roots, per-location bases, and anchors for one schema document.
/// URI keys use exact RFC-resolved spelling; stored JSON/retrieval identity is not
/// rewritten. This does not fetch documents or implement dynamic evaluation scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaResources {
    locations: SchemaLocations,
    bases: Vec<String>,
    resources: BTreeMap<String, usize>,
    anchors: BTreeMap<usize, BTreeMap<String, (usize, bool)>>,
}
impl SchemaResources {
    /// Indexes one document using its absolute, fragment-free retrieval context.
    /// A nested `$id` changes the base and resource root at its actual pointer.
    ///
    /// # Errors
    /// Returns schema-location, URI/anchor, conflicting-identity, or limit errors.
    /// This does not validate all JSON Schema keywords or resolve external closure.
    pub fn new(
        document: &JsonDocument,
        retrieval_uri: &str,
        limits: &Limits,
    ) -> Result<Self, SchemaResourceError> {
        let locations = SchemaLocations::new(document, limits)?;
        text_limit(retrieval_uri, limits)?;
        UriAbsoluteStr::new(retrieval_uri).map_err(|_| SchemaResourceError::InvalidUri)?;
        let mut budget = Accounting {
            limits,
            count: 0,
            bytes: 0,
        };
        let mut result = Self {
            locations,
            bases: Vec::new(),
            resources: BTreeMap::new(),
            anchors: BTreeMap::new(),
        };
        budget.record(retrieval_uri)?;
        result.resources.insert(owned(retrieval_uri)?, 0);
        let mut ancestry: Vec<usize> = Vec::new();
        let mut owners = Vec::new();
        for i in 0..result.locations.pointers().len() {
            let pointer = &result.locations.pointers()[i];
            while ancestry.last().is_some_and(|p| {
                !pointer
                    .tokens()
                    .starts_with(result.locations.pointers()[*p].tokens())
            }) {
                ancestry.pop();
            }
            let parent = ancestry.last().copied();
            let inherited = parent.map_or(retrieval_uri, |p| result.bases[p].as_str());
            let value = pointer.resolve(document)?;
            let declared_id = if let Value::Map(m) = value {
                m.get("$id")
            } else {
                None
            };
            let mut owner = parent.map_or(0, |p| owners[p]);
            let base = if let Some(id) = declared_id {
                let Value::Text(id) = id else {
                    return Err(SchemaResourceError::InvalidKeyword("$id"));
                };
                let mut base = resolve_uri(inherited, id, limits)?;
                if base.ends_with('#') {
                    base.pop();
                }
                UriAbsoluteStr::new(&base)
                    .map_err(|_| SchemaResourceError::InvalidKeyword("$id"))?;
                if let Some(previous) = result.resources.get(&base) {
                    if *previous != i {
                        return Err(SchemaResourceError::ResourceConflict);
                    }
                } else {
                    budget.record(&base)?;
                    result.resources.insert(owned(&base)?, i);
                }
                owner = i;
                base
            } else {
                budget.text(inherited)?;
                owned(inherited)?
            };
            // Declared base storage is in addition to the URI-key copy.
            if declared_id.is_some() {
                budget.text(&base)?;
            }
            budget.entry()?;
            result.bases.try_reserve(1).map_err(allocation)?;
            result.bases.push(base);
            if let Value::Map(m) = value {
                for (key, dynamic) in [("$anchor", false), ("$dynamicAnchor", true)] {
                    if let Some(v) = m.get(key) {
                        let Value::Text(name) = v else {
                            return Err(SchemaResourceError::InvalidKeyword(key));
                        };
                        if !anchor_name(name) {
                            return Err(SchemaResourceError::InvalidKeyword(key));
                        }
                        budget.record(name)?;
                        let entry = result
                            .anchors
                            .entry(owner)
                            .or_default()
                            .entry(owned(name)?)
                            .or_insert((i, false));
                        if entry.0 != i {
                            return Err(SchemaResourceError::AnchorConflict);
                        }
                        entry.1 |= dynamic;
                    }
                }
            }
            owners.try_reserve(1).map_err(allocation)?;
            owners.push(owner);
            ancestry.try_reserve(1).map_err(allocation)?;
            ancestry.push(i);
        }
        Ok(result)
    }
    /// Complete JSON document identity associated with all returned locations.
    pub fn document_digest(&self) -> Digest {
        self.locations.document_digest()
    }
    /// Derived resource URI roots, in exact URI string order.
    pub fn resources(&self) -> impl Iterator<Item = (&str, &JsonPointer)> {
        self.resources
            .iter()
            .map(|(uri, i)| (uri.as_str(), &self.locations.pointers()[*i]))
    }
    /// Effective base URI at a known schema-bearing location.
    pub fn base_uri(&self, location: &JsonPointer) -> Option<&str> {
        self.index(location).map(|i| self.bases[i].as_str())
    }
    pub(crate) fn pointers(&self) -> &[JsonPointer] {
        self.locations.pointers()
    }
    pub(crate) fn has_resource(&self, uri: &str) -> bool {
        self.resources.contains_key(uri)
    }
    /// Resolves an initial reference target from a known schema location. Pointer
    /// fragments start at the selected resource's pointer, not the document root.
    /// Static and dynamic anchors both have an initial target; dynamic rebinding
    /// during instance evaluation is not performed by this method.
    ///
    /// # Errors
    /// Returns unknown locations/resources/anchors, non-schema targets, invalid
    /// URI/pointer syntax, opaque-base restrictions, or construction-limit errors.
    pub fn resolve(
        &self,
        from: &JsonPointer,
        reference: &str,
        limits: &Limits,
    ) -> Result<&JsonPointer, SchemaResourceError> {
        let uri = self.reference_uri(from, reference, limits)?;
        self.resolve_absolute(&uri, limits)
    }
    pub(crate) fn reference_uri(
        &self,
        from: &JsonPointer,
        reference: &str,
        limits: &Limits,
    ) -> Result<String, SchemaResourceError> {
        limits
            .validate()
            .map_err(crate::JsonError::from)
            .map_err(SchemaLocationError::from)?;
        let base = self
            .base_uri(from)
            .ok_or(SchemaResourceError::UnknownLocation)?;
        resolve_uri(base, reference, limits)
    }
    pub(crate) fn resolve_absolute(
        &self,
        uri: &str,
        limits: &Limits,
    ) -> Result<&JsonPointer, SchemaResourceError> {
        let (resource, fragment) = uri.split_once('#').unwrap_or((uri, ""));
        let root = *self
            .resources
            .get(resource)
            .ok_or(SchemaResourceError::MissingResource)?;
        let fragment = decode_fragment(fragment, limits)?;
        let target = if fragment.is_empty() {
            root
        } else if fragment.starts_with('/') {
            let relative = JsonPointer::new(&fragment, limits)?;
            // The joined pointer is not one URI/text token. Its individual token
            // bounds are reapplied by JsonPointer below; bound the temporary by
            // the pointer's encoded-document ceiling.
            let mut pointer_limits = limits.clone();
            pointer_limits.max_text_bytes = limits.max_document_bytes;
            let full = bounded(
                format_args!("{}{}", self.locations.pointers()[root], relative),
                &pointer_limits,
            )?;
            let full = JsonPointer::new(&full, limits)?;
            self.index(&full)
                .ok_or(SchemaResourceError::NonSchemaTarget)?
        } else {
            self.anchors
                .get(&root)
                .and_then(|anchors| anchors.get(&fragment))
                .ok_or(SchemaResourceError::MissingAnchor)?
                .0
        };
        Ok(&self.locations.pointers()[target])
    }
    /// Whether a named anchor in a known resource was declared dynamic.
    /// This records metadata only; it does not resolve evaluation-time scope.
    pub fn is_dynamic_anchor(&self, resource_uri: &str, name: &str) -> bool {
        self.resources.get(resource_uri).is_some_and(|root| {
            self.anchors
                .get(root)
                .and_then(|anchors| anchors.get(name))
                .is_some_and(|(_, dynamic)| *dynamic)
        })
    }
    fn index(&self, p: &JsonPointer) -> Option<usize> {
        self.locations
            .pointers()
            .binary_search_by(|v| v.tokens().cmp(p.tokens()))
            .ok()
    }
    // Account retained child-index metadata before the containing catalog keeps
    // it. A failing child can temporarily occupy one additional bounded index.
    pub(crate) fn stored_metadata(
        &self,
        mut record: impl FnMut(Option<&str>) -> Result<(), SchemaResourceError>,
    ) -> Result<(), SchemaResourceError> {
        for pointer in self.locations.pointers() {
            record(None)?;
            for token in pointer.tokens() {
                record(Some(token))?;
            }
        }
        for text in self.bases.iter().chain(self.resources.keys()) {
            record(Some(text))?;
        }
        for anchors in self.anchors.values() {
            for name in anchors.keys() {
                record(Some(name))?;
            }
        }
        Ok(())
    }
}

fn resolve_uri(base: &str, reference: &str, l: &Limits) -> Result<String, SchemaResourceError> {
    text_limit(base, l)?;
    text_limit(reference, l)?;
    let base = UriAbsoluteStr::new(base).map_err(|_| SchemaResourceError::InvalidUri)?;
    let reference = UriReferenceStr::new(reference).map_err(|_| SchemaResourceError::InvalidUri)?;
    if base.authority_str().is_none()
        && !base.path_str().starts_with('/')
        && !base.path_str().is_empty()
        && reference.scheme_str().is_none()
        && !reference.as_str().is_empty()
        && !reference.as_str().starts_with('#')
    {
        return Err(SchemaResourceError::NonHierarchicalBase);
    }
    // RFC 3986 §5.2, without percent-decoding dot segments or WHATWG URL
    // serialization. Keep external strings exact apart from required resolution.
    let (scheme, authority, path, query) = if let Some(scheme) = reference.scheme_str() {
        (
            scheme,
            reference.authority_str(),
            remove_dots(reference.path_str())?,
            reference.query_str(),
        )
    } else if reference.authority_str().is_some() {
        (
            base.scheme_str(),
            reference.authority_str(),
            remove_dots(reference.path_str())?,
            reference.query_str(),
        )
    } else if reference.path_str().is_empty() {
        (
            base.scheme_str(),
            base.authority_str(),
            owned(base.path_str())?,
            reference.query_str().or(base.query_str()),
        )
    } else {
        let path = if reference.path_str().starts_with('/') {
            remove_dots(reference.path_str())?
        } else {
            let prefix = if base.authority_str().is_some() && base.path_str().is_empty() {
                "/"
            } else {
                &base.path_str()[..base.path_str().rfind('/').map_or(0, |p| p + 1)]
            };
            remove_dots(&bounded(
                format_args!("{prefix}{}", reference.path_str()),
                l,
            )?)?
        };
        (
            base.scheme_str(),
            base.authority_str(),
            path,
            reference.query_str(),
        )
    };
    if authority.is_none() && path.starts_with("//") {
        return Err(SchemaResourceError::InvalidUri);
    }
    let mut output = Output {
        text: String::new(),
        limits: l,
        failure: None,
    };
    let write_result = (|| {
        write!(output, "{scheme}:")?;
        if let Some(authority) = authority {
            write!(output, "//{authority}")?;
        }
        output.write_str(&path)?;
        if let Some(query) = query {
            write!(output, "?{query}")?;
        }
        if let Some(fragment) = reference.fragment_str() {
            write!(output, "#{fragment}")?;
        }
        Ok::<_, fmt::Error>(())
    })();
    if write_result.is_err() {
        return Err(output.failure.unwrap_or(SchemaResourceError::InvalidUri));
    }
    Ok(output.text)
}
fn remove_dots(mut input: &str) -> Result<String, SchemaResourceError> {
    let mut output = String::new();
    output.try_reserve_exact(input.len()).map_err(allocation)?;
    while !input.is_empty() {
        if let Some(rest) = input.strip_prefix("../") {
            input = rest;
        } else if let Some(rest) = input.strip_prefix("./") {
            input = rest;
        } else if input.starts_with("/./") {
            input = &input[2..];
        } else if input == "/." {
            input = "/";
        } else if input.starts_with("/../") || input == "/.." {
            input = if input == "/.." { "/" } else { &input[3..] };
            output.truncate(output.rfind('/').unwrap_or(0));
        } else if input == "." || input == ".." {
            input = "";
        } else {
            let start = usize::from(input.starts_with('/'));
            let end = input[start..].find('/').map_or(input.len(), |p| start + p);
            output.push_str(&input[..end]);
            input = &input[end..];
        }
    }
    Ok(output)
}
fn anchor_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
}
fn decode_fragment(s: &str, l: &Limits) -> Result<String, SchemaResourceError> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(s.len()).map_err(allocation)?;
    let mut pos = 0;
    while pos < s.len() {
        let b = s.as_bytes()[pos];
        if b == b'%' {
            let hex = s
                .get(pos + 1..pos + 3)
                .ok_or(SchemaResourceError::InvalidUri)?;
            bytes.push(u8::from_str_radix(hex, 16).map_err(|_| SchemaResourceError::InvalidUri)?);
            pos += 3;
        } else {
            bytes.push(b);
            pos += 1;
        }
    }
    let decoded = String::from_utf8(bytes).map_err(|_| SchemaResourceError::InvalidUri)?;
    text_limit(&decoded, l)?;
    Ok(decoded)
}
struct Output<'a> {
    text: String,
    limits: &'a Limits,
    failure: Option<SchemaResourceError>,
}
impl fmt::Write for Output<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let result =
            (|| {
                let length = self.text.len().checked_add(s.len()).ok_or(
                    SchemaResourceError::LimitExceeded {
                        limit: LimitKind::DocumentBytes,
                        maximum: self.limits.max_document_bytes,
                    },
                )?;
                check(length, self.limits.max_text_bytes, LimitKind::TextBytes)?;
                check(
                    length,
                    self.limits.max_document_bytes,
                    LimitKind::DocumentBytes,
                )?;
                check(
                    length,
                    self.limits.max_total_payload_bytes,
                    LimitKind::TotalPayloadBytes,
                )?;
                self.text.try_reserve(s.len()).map_err(allocation)?;
                self.text.push_str(s);
                Ok(())
            })();
        if let Err(error) = result {
            self.failure = Some(error);
            Err(fmt::Error)
        } else {
            Ok(())
        }
    }
}
fn bounded(v: impl fmt::Display, l: &Limits) -> Result<String, SchemaResourceError> {
    let mut output = Output {
        text: String::new(),
        limits: l,
        failure: None,
    };
    if write!(output, "{v}").is_err() {
        return Err(output.failure.unwrap_or(SchemaResourceError::InvalidUri));
    }
    Ok(output.text)
}
fn text_limit(s: &str, l: &Limits) -> Result<(), SchemaResourceError> {
    check(s.len(), l.max_text_bytes, LimitKind::TextBytes)?;
    check(s.len(), l.max_document_bytes, LimitKind::DocumentBytes)?;
    check(
        s.len(),
        l.max_total_payload_bytes,
        LimitKind::TotalPayloadBytes,
    )
}
fn check(n: usize, maximum: usize, limit: LimitKind) -> Result<(), SchemaResourceError> {
    if n > maximum {
        Err(SchemaResourceError::LimitExceeded { limit, maximum })
    } else {
        Ok(())
    }
}
fn owned(s: &str) -> Result<String, SchemaResourceError> {
    let mut text = String::new();
    text.try_reserve_exact(s.len()).map_err(allocation)?;
    text.push_str(s);
    Ok(text)
}
fn allocation(_: std::collections::TryReserveError) -> SchemaResourceError {
    SchemaResourceError::AllocationFailed
}
struct Accounting<'a> {
    limits: &'a Limits,
    count: usize,
    bytes: usize,
}
impl Accounting<'_> {
    fn entry(&mut self) -> Result<(), SchemaResourceError> {
        self.count = self
            .count
            .checked_add(1)
            .ok_or(SchemaResourceError::LimitExceeded {
                limit: LimitKind::TotalValues,
                maximum: self.limits.max_total_values,
            })?;
        check(
            self.count,
            self.limits.max_total_values,
            LimitKind::TotalValues,
        )?;
        check(
            self.count,
            self.limits.max_collection_entries,
            LimitKind::CollectionEntries,
        )
    }
    fn text(&mut self, text: &str) -> Result<(), SchemaResourceError> {
        text_limit(text, self.limits)?;
        self.bytes =
            self.bytes
                .checked_add(text.len())
                .ok_or(SchemaResourceError::LimitExceeded {
                    limit: LimitKind::TotalPayloadBytes,
                    maximum: self.limits.max_total_payload_bytes,
                })?;
        check(
            self.bytes,
            self.limits.max_total_payload_bytes,
            LimitKind::TotalPayloadBytes,
        )
    }
    fn record(&mut self, text: &str) -> Result<(), SchemaResourceError> {
        self.entry()?;
        self.text(text)
    }
}

/// Resource-index errors with static, input-free diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaResourceError {
    /// Schema location discovery failed.
    Locations(SchemaLocationError),
    /// A pointer fragment failed parsing or lookup.
    Pointer(JsonPointerError),
    /// Invalid absolute URI, reference, UTF-8 fragment, or RFC resolution result.
    InvalidUri,
    /// Invalid resource/anchor declaration field.
    InvalidKeyword(&'static str),
    /// A non-fragment relative reference lacks a hierarchical base.
    NonHierarchicalBase,
    /// Different locations claim one resource URI.
    ResourceConflict,
    /// One retrieval URI is supplied with differing canonical documents.
    RetrievalConflict,
    /// Reference source retrieval URI is absent from the catalog.
    UnknownDocument,
    /// A schema reference keyword contains a non-string value.
    InvalidReference(&'static str),
    /// Different locations claim one anchor within a resource.
    AnchorConflict,
    /// Reference source is not a discovered schema location.
    UnknownLocation,
    /// Target resource is absent from this per-document index.
    MissingResource,
    /// Selected resource has no matching named anchor.
    MissingAnchor,
    /// Pointer does not select a discovered schema location.
    NonSchemaTarget,
    /// Derived metadata or URI construction exceeds a ceiling.
    LimitExceeded {
        /// Exhausted resource.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// Storage reservation failed.
    AllocationFailed,
}
impl From<SchemaLocationError> for SchemaResourceError {
    fn from(e: SchemaLocationError) -> Self {
        Self::Locations(e)
    }
}
impl From<JsonPointerError> for SchemaResourceError {
    fn from(e: JsonPointerError) -> Self {
        Self::Pointer(e)
    }
}
impl fmt::Display for SchemaResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "schema resource error: {self:?}")
    }
}
impl std::error::Error for SchemaResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Locations(e) => Some(e),
            Self::Pointer(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rfc_3986_reference_resolution_vectors() {
        let base = "http://a/b/c/d;p?q";
        for (reference, expected) in [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("g?y#s", "http://a/b/c/g?y#s"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            ("g..", "http://a/b/c/g.."),
            ("./../g", "http://a/b/g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("g?y/../x", "http://a/b/c/g?y/../x"),
            ("g#s/./x", "http://a/b/c/g#s/./x"),
            ("http:g", "http:g"),
            ("%2e%2e/g", "http://a/b/c/%2e%2e/g"),
        ] {
            assert_eq!(
                resolve_uri(base, reference, &Limits::default()).unwrap(),
                expected,
                "{reference}"
            );
        }
    }
}

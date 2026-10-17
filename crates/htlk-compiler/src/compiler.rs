//! Compiler configuration and common final artifact admission.
use crate::executable::{cbor::Limits, digest::Digest};
use crate::{Diagnostic, SourceMapEntry};
use crate::{
    MCPCatalogInputs,
    analyzer::{NativeSchemaOptions, analyze_document},
    diagnostic::{Result, analysis, fail},
    executable::*,
};
use std::collections::BTreeMap;

/// Frontend ceilings, separate from per-record codec/analysis ceilings.
#[derive(Clone, Debug)]
pub struct FrontendLimits {
    /// Aggregate source UTF-8 bytes.
    pub max_source_bytes: usize,
    /// Total supplied packages.
    pub max_packages: usize,
    /// Total mapped modules.
    pub max_modules: usize,
    /// Maximum dependency/import/recursive frontend depth.
    pub max_depth: usize,
    /// Tokens per request.
    pub max_tokens: usize,
    /// Maximum report/provenance bytes.
    pub max_report_bytes: usize,
    /// Aggregate frontend traversal work.
    pub max_work: usize,
}
impl Default for FrontendLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 16 * 1024 * 1024,
            max_packages: 1024,
            max_modules: 4096,
            max_depth: 64,
            max_tokens: 1_000_000,
            max_report_bytes: 16 * 1024 * 1024,
            max_work: 10_000_000,
        }
    }
}

/// Trusted, fixed compiler build/host selections. Input source cannot modify them.
#[derive(Clone, Debug)]
pub struct CompilerConfig {
    /// Exact linked interpretation.
    pub profile: ExecutionProfile,
    /// Exact selected policy bytes.
    pub policy: PolicyDocument,
    /// Complete linked library manifests keyed by implementation digest.
    pub libraries: BTreeMap<Digest, Library>,
    /// Executable and derived analysis limits.
    pub limits: Limits,
    /// Native schema backend controls.
    pub schema_options: NativeSchemaOptions,
    /// Source and report ceilings.
    pub frontend: FrontendLimits,
}

/// Complete checked compiler output. Reports do not affect executable identity.
#[derive(Clone, Debug)]
pub struct CompilationSuccess {
    /// Authoritative normalized executable document.
    pub canonical_document: CanonicalDocument,
    /// Deterministic fingerprinted CBOR envelope.
    pub executable_bytes: Vec<u8>,
    /// Envelope identity.
    pub fingerprint: Digest,
    /// Nonfatal semantic reports.
    pub diagnostics: Vec<Diagnostic>,
    /// Source associations; empty for canonical input.
    pub source_map: Vec<SourceMapEntry>,
    /// Retained authored descriptions keyed by canonical pointer.
    pub documentation: BTreeMap<String, String>,
}

/// Reusable compiler with immutable trusted configuration; performs no I/O.
pub struct Compiler {
    pub(crate) config: CompilerConfig,
}

/// The three normative compilation input forms.
pub enum CompilationInput<'a> {
    /// One standalone source string.
    SourceText(&'a str),
    /// Finite package snapshots and selected entry.
    SourceBundle(&'a crate::SourceBundle),
    /// An untrusted normalized document to revalidate.
    CanonicalDocument(CanonicalDocument),
}
impl<'a> From<&'a str> for CompilationInput<'a> {
    fn from(v: &'a str) -> Self {
        Self::SourceText(v)
    }
}
impl<'a> From<&'a crate::SourceBundle> for CompilationInput<'a> {
    fn from(v: &'a crate::SourceBundle) -> Self {
        Self::SourceBundle(v)
    }
}
impl From<CanonicalDocument> for CompilationInput<'_> {
    fn from(v: CanonicalDocument) -> Self {
        Self::CanonicalDocument(v)
    }
}
impl Compiler {
    /// Compiles source, a finite source bundle, or a canonical document.
    ///
    /// # Errors
    /// Returns structured diagnostics; failure never contains a partial executable.
    pub fn compile_ir<'a>(
        &self,
        input: impl Into<CompilationInput<'a>>,
        catalogs: &MCPCatalogInputs,
    ) -> Result<CompilationSuccess> {
        match input.into() {
            CompilationInput::SourceText(s) => self.compile_source(s, catalogs),
            CompilationInput::SourceBundle(b) => self.compile_bundle(b, catalogs),
            CompilationInput::CanonicalDocument(d) => self.compile_document(d, catalogs),
        }
    }
    /// Validates fixed compiler selections and resource ceilings.
    ///
    /// # Errors
    /// Rejects mismatched policy identities, invalid manifests/options or limits.
    pub fn new(config: CompilerConfig) -> Result<Self> {
        config.limits.validate().map_err(|e| fail("C_POLICY", e))?;
        config
            .profile
            .to_value(&config.limits)
            .map_err(|e| fail("C_POLICY", e))?;
        if config.profile.policy_document() != config.policy.digest() {
            return Err(fail(
                "C_POLICY",
                "profile does not select configured policy",
            ));
        }
        let f = &config.frontend;
        if [
            f.max_source_bytes,
            f.max_packages,
            f.max_modules,
            f.max_depth,
            f.max_tokens,
            f.max_report_bytes,
            f.max_work,
            config.schema_options.max_pattern_bytes,
            config.schema_options.max_regex_compiled_bytes,
        ]
        .contains(&0)
            || f.max_depth > 128
        {
            return Err(fail("C_POLICY", "invalid frontend/schema limits"));
        }
        for (id, library) in &config.libraries {
            if id != &library.implementation_digest() {
                return Err(fail("C_LIBRARY", "incorrect linked manifest key"));
            }
            library
                .to_value(&config.limits)
                .map_err(|e| fail("C_LIBRARY", e))?;
            for (_, signature) in library.functions() {
                crate::analyzer::check_function_signature(signature, &config.limits)
                    .map_err(|e| fail("C_LIBRARY", e))?;
            }
        }
        Ok(Self { config })
    }
    /// Revalidates canonical input against this compiler and current catalogs.
    ///
    /// # Errors
    /// Returns exact-profile/catalog/manifest, semantic or serialization diagnostics.
    pub fn compile_document(
        &self,
        document: CanonicalDocument,
        catalogs: &MCPCatalogInputs,
    ) -> Result<CompilationSuccess> {
        let l = &self.config.limits;
        let f = document.fields();
        if f.profile != self.config.profile
            || f.documents.get(&self.config.policy.digest()) != Some(self.config.policy.document())
        {
            return Err(fail(
                "C_POLICY",
                "document does not match fixed compiler profile/policy",
            ));
        }
        for (id, library) in &f.libraries {
            if self.config.libraries.get(id) != Some(library) {
                return Err(fail(
                    "C_LIBRARY",
                    "document manifest is not the complete linked library",
                ));
            }
        }
        for binding in f.bindings.values() {
            catalogs.descriptor(binding, l)?;
        }
        for (uri, id) in &f.schema_uris {
            // Embedded schema roots are checked against descriptor subdocuments by
            // complete analysis; externally retrieved roots must match catalogs.
            let embedded = f.bindings.values().any(|b| match b.kind() {
                McpBindingKind::Tool {
                    input_schema,
                    output_schema,
                    ..
                } => id == input_schema || id == output_schema,
                _ => false,
            });
            if !embedded && catalogs.schemas.get(uri) != f.documents.get(id) {
                return Err(fail(
                    "C_SCHEMA",
                    "external schema snapshot differs from supplied catalogs",
                ));
            }
        }
        // Analysis consumes its input. Retain the bounded representation for
        // structured error conversion; it never becomes a second authority.
        let analyzed = analyze_document(document.clone(), self.config.schema_options, l).map_err(
            |e| match e.diagnostic(&document, l) {
                Ok(d) => crate::CompilationFailure {
                    diagnostics: vec![analysis(d)],
                },
                Err(cause) => fail("C_SERIALIZATION", cause),
            },
        )?;
        let diagnostics = analyzed
            .diagnostics()
            .iter()
            .cloned()
            .map(analysis)
            .collect();
        let envelope = analyzed
            .document()
            .envelope(l)
            .map_err(|e| fail("C_SERIALIZATION", e))?;
        let executable_bytes = envelope.encode(l).map_err(|e| fail("C_SERIALIZATION", e))?;
        Ok(CompilationSuccess {
            canonical_document: document,
            fingerprint: envelope.fingerprint(),
            executable_bytes,
            diagnostics,
            source_map: vec![],
            documentation: BTreeMap::new(),
        })
    }
}

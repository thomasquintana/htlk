//! Canonical execution-profile, library, and MCP identity metadata.

use std::{collections::BTreeSet, fmt, fmt::Write as _};

use crate::cbor as htlk_cbor;
use htlk_cbor::{LimitKind, Limits, Map, Value};

use crate::digest::{Digest, ParseDigestError, RecordKind, record_digest};
use crate::record_accounting::{EncodingLimitError, RecordAccounting};
use crate::{Identifier, ParseIdentifierError, Port, TypeContext, TypeError};

/// Supported HTLK evaluator-core format version.
pub const CORE_VERSION: &str = "0.1";
/// MCP protocol version pinned by the current executable profile schema.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Canonical metadata shape, version, signature, or conversion failure.
/// Errors contain static schema terms, not submitted strings or credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MetadataError {
    /// Underlying codec failure, retaining original offsets.
    Codec(htlk_cbor::Error),
    /// Invalid canonical identifier.
    Identifier(ParseIdentifierError),
    /// Invalid digest representation.
    Digest(ParseDigestError),
    /// Invalid signature type/port record.
    Type(TypeError),
    /// Incorrect shape or native field type at a fixed schema site.
    InvalidShape(&'static str),
    /// Required metadata field absent.
    MissingField(&'static str),
    /// Unknown field in the named record.
    UnknownField(&'static str),
    /// Unsupported HTLK core version.
    UnsupportedCoreVersion,
    /// Unsupported pinned MCP protocol version.
    UnsupportedMcpVersion,
    /// Unsupported transport spelling.
    UnsupportedTransport,
    /// Unknown MCP binding kind.
    UnknownBindingKind,
    /// Repeated generic-variable declaration.
    DuplicateTypeParameter,
    /// Signature references a generic variable outside its declarations.
    UndeclaredTypeVariable,
    /// Metadata conversion exceeds a codec ceiling.
    LimitExceeded {
        /// Exhausted resource.
        limit: LimitKind,
        /// Configured ceiling.
        maximum: usize,
    },
    /// Fallible storage reservation failed.
    AllocationFailed,
}
macro_rules! error_from {
    ($t:ty, $variant:ident) => {
        impl From<$t> for MetadataError {
            fn from(e: $t) -> Self {
                Self::$variant(e)
            }
        }
    };
}
error_from!(htlk_cbor::Error, Codec);
error_from!(ParseIdentifierError, Identifier);
error_from!(ParseDigestError, Digest);
error_from!(TypeError, Type);
impl From<EncodingLimitError> for MetadataError {
    fn from(e: EncodingLimitError) -> Self {
        Self::LimitExceeded {
            limit: e.limit,
            maximum: e.maximum,
        }
    }
}
impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(e) => write!(f, "execution metadata: {e}"),
            Self::Identifier(e) => write!(f, "metadata identifier: {e}"),
            Self::Digest(e) => write!(f, "metadata digest: {e}"),
            Self::Type(e) => write!(f, "signature metadata: {e}"),
            Self::InvalidShape(site) => write!(f, "invalid metadata shape: {site}"),
            Self::MissingField(field) => write!(f, "missing metadata field: {field}"),
            Self::UnknownField(record) => write!(f, "unknown field in metadata record: {record}"),
            Self::UnsupportedCoreVersion => f.write_str("unsupported evaluator-core version"),
            Self::UnsupportedMcpVersion => f.write_str("unsupported MCP protocol version"),
            Self::UnsupportedTransport => f.write_str("unsupported MCP transport"),
            Self::UnknownBindingKind => f.write_str("unknown MCP binding kind"),
            Self::DuplicateTypeParameter => f.write_str("duplicate generic-variable declaration"),
            Self::UndeclaredTypeVariable => f.write_str("undeclared generic variable in signature"),
            Self::LimitExceeded { limit, maximum } => {
                write!(f, "metadata limit exceeded: {limit:?} ({maximum})")
            }
            Self::AllocationFailed => f.write_str("metadata allocation failed"),
        }
    }
}
impl std::error::Error for MetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            Self::Identifier(e) => Some(e),
            Self::Digest(e) => Some(e),
            Self::Type(e) => Some(e),
            _ => None,
        }
    }
}

trait Record: Sized {
    fn build(&self, limits: &Limits) -> Result<Value, MetadataError>;
    fn parse(value: &Value, limits: &Limits) -> Result<Self, MetadataError>;
}
macro_rules! codec_methods {
    ($ty:ty) => {
        impl $ty {
            /// Produces a bounded canonical metadata record.
            ///
            /// # Errors
            /// Returns schema, signature, conversion-limit, or allocation failures.
            pub fn to_value(&self, limits: &Limits) -> Result<Value, MetadataError> {
                self.build(limits)
            }
            /// Parses already decoded canonical metadata without coercion/defaults.
            ///
            /// # Errors
            /// Returns codec, schema, version, identity, or signature failures.
            pub fn from_value(value: &Value, limits: &Limits) -> Result<Self, MetadataError> {
                htlk_cbor::encode(value, limits)?;
                Self::parse(value, limits)
            }
            /// Encodes exactly one canonical metadata record.
            ///
            /// # Errors
            /// Returns conversion-limit or allocation failures.
            pub fn encode(&self, limits: &Limits) -> Result<Vec<u8>, MetadataError> {
                Ok(htlk_cbor::encode(&self.to_value(limits)?, limits)?)
            }
            /// Decodes exactly one canonical metadata record, rejecting trailing data.
            ///
            /// # Errors
            /// Returns codec, schema, version, identity, or signature failures.
            pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Self, MetadataError> {
                Self::parse(&htlk_cbor::decode(bytes, limits)?, limits)
            }
        }
    };
}

/// Exact engine implementation/data identity, not a running engine instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineIdentity {
    name: String,
    version: String,
    data_version: String,
    implementation_digest: Digest,
}
impl EngineIdentity {
    /// Preserves exact engine strings and verifies codec bounds.
    ///
    /// # Errors
    /// Returns codec/resource failures. Availability/trust require linked engines.
    pub fn new(
        name: String,
        version: String,
        data_version: String,
        implementation_digest: Digest,
        limits: &Limits,
    ) -> Result<Self, MetadataError> {
        let result = Self {
            name,
            version,
            data_version,
            implementation_digest,
        };
        result.to_value(limits)?;
        Ok(result)
    }
    /// Exact engine name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Exact external implementation version; no SemVer rewriting occurs.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Exact data-table/version identity, including an empty string when supplied.
    pub fn data_version(&self) -> &str {
        &self.data_version
    }
    /// Claimed implementation digest, matched against linked implementations later.
    pub const fn implementation_digest(&self) -> Digest {
        self.implementation_digest
    }
}
codec_methods!(EngineIdentity);

/// Pinned core/protocol/engine/policy metadata. Construction does not load engines
/// or verify the referenced policy document or linked implementation digests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionProfile {
    core_digest: Digest,
    regex_engine: EngineIdentity,
    schema_validator: EngineIdentity,
    uri_template_engine: EngineIdentity,
    policy_document: Digest,
}
impl ExecutionProfile {
    /// Creates the supported core-0.1/MCP-2025-11-25 profile, bounding the complete
    /// embedded engine records. External engine version strings are preserved.
    ///
    /// # Errors
    /// Returns codec/resource failures.
    pub fn new(
        core_digest: Digest,
        regex_engine: EngineIdentity,
        schema_validator: EngineIdentity,
        uri_template_engine: EngineIdentity,
        policy_document: Digest,
        limits: &Limits,
    ) -> Result<Self, MetadataError> {
        let result = Self {
            core_digest,
            regex_engine,
            schema_validator,
            uri_template_engine,
            policy_document,
        };
        result.to_value(limits)?;
        Ok(result)
    }
    /// Supported evaluator-core format version.
    pub const fn core_version(&self) -> &'static str {
        CORE_VERSION
    }
    /// Claimed evaluator-core implementation identity.
    pub const fn core_digest(&self) -> Digest {
        self.core_digest
    }
    /// Supported pinned MCP protocol version.
    pub const fn mcp_protocol_version(&self) -> &'static str {
        MCP_PROTOCOL_VERSION
    }
    /// Regex engine identity.
    pub fn regex_engine(&self) -> &EngineIdentity {
        &self.regex_engine
    }
    /// Schema validator identity.
    pub fn schema_validator(&self) -> &EngineIdentity {
        &self.schema_validator
    }
    /// URI-template engine identity.
    pub fn uri_template_engine(&self) -> &EngineIdentity {
        &self.uri_template_engine
    }
    /// Referenced policy JCS document digest.
    pub const fn policy_document(&self) -> Digest {
        self.policy_document
    }
}
codec_methods!(ExecutionProfile);

/// Rank-one signature metadata, with declared variables and positional ports.
/// Declaration uniqueness is checked here; variable resolution and call-site
/// unification, recursive inference, and implementation compatibility are not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionSignature {
    type_parameters: Vec<Identifier>,
    parameters: Vec<Port>,
    returns: Port,
}
impl FunctionSignature {
    /// Bounds signature data and checks duplicate declarations. The analyzer
    /// resolves variable uses against those declarations.
    /// Parameter and type-parameter declaration order is preserved.
    ///
    /// # Errors
    /// Returns duplicate declaration, type representation, or resource failures.
    pub fn new(
        type_parameters: Vec<Identifier>,
        parameters: Vec<Port>,
        returns: Port,
        limits: &Limits,
    ) -> Result<Self, MetadataError> {
        let result = Self {
            type_parameters,
            parameters,
            returns,
        };
        result.to_value(limits)?;
        Ok(result)
    }
    /// Declared generic variables, in declaration order.
    pub fn type_parameters(&self) -> &[Identifier] {
        &self.type_parameters
    }
    /// Positional parameter ports, including their presence rules.
    pub fn parameters(&self) -> &[Port] {
        &self.parameters
    }
    /// Result type and presence rule.
    pub fn returns(&self) -> &Port {
        &self.returns
    }
}
codec_methods!(FunctionSignature);

/// Complete supplied public library signature manifest. Its implementation digest
/// identifies linked implementation metadata, not a hash of this caller record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Library {
    library_id: String,
    version: String,
    implementation_digest: Digest,
    functions: Vec<(Identifier, FunctionSignature)>,
}
impl Library {
    /// Validates a supplied manifest and normalizes function-map iteration order.
    /// No functions are pruned and no implementation identity is recomputed.
    ///
    /// # Errors
    /// Returns duplicate-name, signature, or resource failures.
    pub fn new(
        library_id: String,
        version: String,
        implementation_digest: Digest,
        functions: Vec<(Identifier, FunctionSignature)>,
        limits: &Limits,
    ) -> Result<Self, MetadataError> {
        let mut result = Self {
            library_id,
            version,
            implementation_digest,
            functions,
        };
        result.to_value(limits)?;
        result.functions.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Ok(result)
    }
    /// Exact library registry ID.
    pub fn library_id(&self) -> &str {
        &self.library_id
    }
    /// Exact manifest version, not a version-selection range.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Claimed linked implementation identity.
    pub const fn implementation_digest(&self) -> Digest {
        self.implementation_digest
    }
    /// Public signatures, exposed in decoded UTF-8 name order.
    pub fn functions(&self) -> &[(Identifier, FunctionSignature)] {
        &self.functions
    }
    /// Looks up an exact function name without interpretation or invocation.
    pub fn function(&self, name: &str) -> Option<&FunctionSignature> {
        self.functions
            .binary_search_by(|(id, _)| id.as_str().cmp(name))
            .ok()
            .map(|i| &self.functions[i].1)
    }
}
codec_methods!(Library);

/// The closed MCP transport identities used by the executable format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpTransport {
    /// Child-process standard input/output streams.
    Stdio,
    /// MCP streamable HTTP transport.
    StreamableHttp,
}
impl McpTransport {
    /// Exact canonical spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp => "streamable_http",
        }
    }
    fn parse(s: &str) -> Result<Self, MetadataError> {
        match s {
            "stdio" => Ok(Self::Stdio),
            "streamable_http" => Ok(Self::StreamableHttp),
            _ => Err(MetadataError::UnsupportedTransport),
        }
    }
}

/// Compound server selection metadata. Endpoints, credentials, observations,
/// aliases, and connection handles are deliberately not fields of this record.
/// Deployment selection must still come from trusted host configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerIdentity {
    deployment_id: String,
    transport: McpTransport,
    implementation_name: String,
    implementation_version: String,
}
impl ServerIdentity {
    /// Preserves exact identity strings and checks canonical encoding bounds.
    ///
    /// # Errors
    /// Returns codec/resource failures; no connection is opened or authenticated.
    pub fn new(
        deployment_id: String,
        transport: McpTransport,
        implementation_name: String,
        implementation_version: String,
        limits: &Limits,
    ) -> Result<Self, MetadataError> {
        let result = Self {
            deployment_id,
            transport,
            implementation_name,
            implementation_version,
        };
        result.to_value(limits)?;
        Ok(result)
    }
    /// Exact host deployment ID.
    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }
    /// Selected transport kind.
    pub const fn transport(&self) -> McpTransport {
        self.transport
    }
    /// Exact server implementation name.
    pub fn implementation_name(&self) -> &str {
        &self.implementation_name
    }
    /// Exact server implementation version.
    pub fn implementation_version(&self) -> &str {
        &self.implementation_version
    }
    /// Computes record_digest("server", record), not producer authentication.
    ///
    /// # Errors
    /// Returns codec/resource failures.
    pub fn digest(&self, l: &Limits) -> Result<Digest, MetadataError> {
        Ok(record_digest(RecordKind::Server, &self.to_value(l)?, l)?)
    }
}
codec_methods!(ServerIdentity);

/// The selected MCP descriptor kind and its kind-specific executable fields.
/// External names/URIs are exact strings; there is no universal URI normalizer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpBindingKind {
    /// Callable tool with exact input/output schema document digests.
    Tool {
        /// Exact tool name.
        name: String,
        /// Input schema root digest.
        input_schema: Digest,
        /// Output schema root digest.
        output_schema: Digest,
    },
    /// A fixed resource selection.
    Resource {
        /// Exact resource URI.
        uri: String,
    },
    /// A parameterized resource selection.
    Template {
        /// Exact URI-template text.
        uri_template: String,
    },
    /// A server-side prompt selection, distinct from a local PromptTemplate.
    Prompt {
        /// Exact prompt name.
        name: String,
    },
}

/// A pinned MCP descriptor selection. Descriptor/schema integrity and live
/// connection authorization require later catalog/profile/runtime checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpBinding {
    server: ServerIdentity,
    descriptor: Digest,
    kind: McpBindingKind,
}
impl McpBinding {
    /// Creates one closed kind-specific binding record under codec limits.
    ///
    /// # Errors
    /// Returns codec/resource failures. It does not fetch or validate descriptors.
    pub fn new(
        server: ServerIdentity,
        descriptor: Digest,
        kind: McpBindingKind,
        limits: &Limits,
    ) -> Result<Self, MetadataError> {
        let result = Self {
            server,
            descriptor,
            kind,
        };
        result.to_value(limits)?;
        Ok(result)
    }
    /// Compound server identity.
    pub fn server(&self) -> &ServerIdentity {
        &self.server
    }
    /// Exact descriptor document digest.
    pub const fn descriptor(&self) -> Digest {
        self.descriptor
    }
    /// Borrowed kind-specific selection fields.
    pub fn kind(&self) -> &McpBindingKind {
        &self.kind
    }
    /// Computes record_digest("binding", record).
    ///
    /// # Errors
    /// Returns codec/resource failures.
    pub fn digest(&self, l: &Limits) -> Result<Digest, MetadataError> {
        Ok(record_digest(RecordKind::Binding, &self.to_value(l)?, l)?)
    }
}
codec_methods!(McpBinding);

fn allocation(_: std::collections::TryReserveError) -> MetadataError {
    MetadataError::AllocationFailed
}
fn owned(s: &str) -> Result<String, MetadataError> {
    let mut v = String::new();
    v.try_reserve_exact(s.len()).map_err(allocation)?;
    v.push_str(s);
    Ok(v)
}
fn push<T>(v: &mut Vec<T>, item: T) -> Result<(), MetadataError> {
    v.try_reserve(1).map_err(allocation)?;
    v.push(item);
    Ok(())
}
fn digest_text(d: Digest) -> Result<String, MetadataError> {
    let mut s = String::new();
    s.try_reserve_exact(71).map_err(allocation)?;
    write!(&mut s, "{d}").expect("String formatting cannot fail");
    Ok(s)
}
fn text<'a>(v: &'a Value, site: &'static str) -> Result<&'a str, MetadataError> {
    match v {
        Value::Text(s) => Ok(s),
        _ => Err(MetadataError::InvalidShape(site)),
    }
}
fn array<'a>(v: &'a Value, site: &'static str) -> Result<&'a [Value], MetadataError> {
    match v {
        Value::Array(v) => Ok(v),
        _ => Err(MetadataError::InvalidShape(site)),
    }
}
fn map<'a>(v: &'a Value, site: &'static str) -> Result<&'a Map, MetadataError> {
    match v {
        Value::Map(v) => Ok(v),
        _ => Err(MetadataError::InvalidShape(site)),
    }
}
fn closed<'a>(
    v: &'a Value,
    name: &'static str,
    fields: &[&'static str],
) -> Result<&'a Map, MetadataError> {
    let m = map(v, name)?;
    if m.iter().any(|(k, _)| !fields.contains(&k)) {
        return Err(MetadataError::UnknownField(name));
    }
    for field in fields {
        if m.get(field).is_none() {
            return Err(MetadataError::MissingField(field));
        }
    }
    Ok(m)
}
fn field<'a>(m: &'a Map, key: &str) -> &'a Value {
    m.get(key).expect("required fields checked")
}
fn string(m: &Map, key: &'static str) -> Result<String, MetadataError> {
    owned(text(field(m, key), key)?)
}
fn digest(m: &Map, key: &'static str) -> Result<Digest, MetadataError> {
    Ok(text(field(m, key), key)?.parse()?)
}

struct Builder<'a> {
    accounting: RecordAccounting<'a>,
    fields: Vec<(String, Value)>,
}
impl<'a> Builder<'a> {
    fn new(count: usize, l: &'a Limits) -> Result<Self, MetadataError> {
        let mut accounting = RecordAccounting::new(l)?;
        accounting.collection(count, 0)?;
        Ok(Self {
            accounting,
            fields: Vec::new(),
        })
    }
    fn text(&mut self, key: &str, value: &str) -> Result<(), MetadataError> {
        self.accounting.text(key, 1)?;
        self.accounting.text(value, 1)?;
        push(&mut self.fields, (owned(key)?, Value::Text(owned(value)?)))
    }
    fn digest(&mut self, key: &str, value: Digest) -> Result<(), MetadataError> {
        self.text(key, &digest_text(value)?)
    }
    fn child(&mut self, key: &str, value: Value) -> Result<(), MetadataError> {
        self.accounting.text(key, 1)?;
        self.accounting.value(&value, 1)?;
        push(&mut self.fields, (owned(key)?, value))
    }
    fn finish(self) -> Result<Value, MetadataError> {
        Ok(Value::Map(Map::try_from_entries(self.fields)?))
    }
}

impl Record for EngineIdentity {
    fn build(&self, l: &Limits) -> Result<Value, MetadataError> {
        let mut b = Builder::new(4, l)?;
        b.text("name", &self.name)?;
        b.text("version", &self.version)?;
        b.text("data_version", &self.data_version)?;
        b.digest("implementation_digest", self.implementation_digest)?;
        b.finish()
    }
    fn parse(v: &Value, _: &Limits) -> Result<Self, MetadataError> {
        let m = closed(
            v,
            "engine",
            &["data_version", "implementation_digest", "name", "version"],
        )?;
        Ok(Self {
            name: string(m, "name")?,
            version: string(m, "version")?,
            data_version: string(m, "data_version")?,
            implementation_digest: digest(m, "implementation_digest")?,
        })
    }
}
impl Record for ExecutionProfile {
    fn build(&self, l: &Limits) -> Result<Value, MetadataError> {
        let mut b = Builder::new(7, l)?;
        b.text("core_version", CORE_VERSION)?;
        b.digest("core_digest", self.core_digest)?;
        b.text("mcp_protocol_version", MCP_PROTOCOL_VERSION)?;
        b.child("regex_engine", self.regex_engine.to_value(l)?)?;
        b.child("schema_validator", self.schema_validator.to_value(l)?)?;
        b.child("uri_template_engine", self.uri_template_engine.to_value(l)?)?;
        b.digest("policy_document", self.policy_document)?;
        b.finish()
    }
    fn parse(v: &Value, l: &Limits) -> Result<Self, MetadataError> {
        let m = closed(
            v,
            "profile",
            &[
                "core_digest",
                "core_version",
                "mcp_protocol_version",
                "policy_document",
                "regex_engine",
                "schema_validator",
                "uri_template_engine",
            ],
        )?;
        if text(field(m, "core_version"), "core_version")? != CORE_VERSION {
            return Err(MetadataError::UnsupportedCoreVersion);
        }
        if text(field(m, "mcp_protocol_version"), "mcp_protocol_version")? != MCP_PROTOCOL_VERSION {
            return Err(MetadataError::UnsupportedMcpVersion);
        }
        Ok(Self {
            core_digest: digest(m, "core_digest")?,
            policy_document: digest(m, "policy_document")?,
            regex_engine: EngineIdentity::parse(field(m, "regex_engine"), l)?,
            schema_validator: EngineIdentity::parse(field(m, "schema_validator"), l)?,
            uri_template_engine: EngineIdentity::parse(field(m, "uri_template_engine"), l)?,
        })
    }
}
impl Record for FunctionSignature {
    fn build(&self, l: &Limits) -> Result<Value, MetadataError> {
        let mut b = Builder::new(3, l)?;
        b.accounting.text("type_parameters", 1)?;
        b.accounting.collection(self.type_parameters.len(), 1)?;
        let mut names = Vec::new();
        for name in &self.type_parameters {
            b.accounting.text(name.as_str(), 2)?;
            push(&mut names, Value::Text(owned(name.as_str())?))?;
        }
        push(
            &mut b.fields,
            (owned("type_parameters")?, Value::Array(names)),
        )?;
        b.accounting.text("parameters", 1)?;
        b.accounting.collection(self.parameters.len(), 1)?;
        let mut params = Vec::new();
        for port in &self.parameters {
            let value = port.to_value(TypeContext::Signature, l)?;
            b.accounting.value(&value, 2)?;
            push(&mut params, value)?;
        }
        push(&mut b.fields, (owned("parameters")?, Value::Array(params)))?;
        b.child("returns", self.returns.to_value(TypeContext::Signature, l)?)?;
        self.validate_declarations()?;
        b.finish()
    }
    fn parse(v: &Value, l: &Limits) -> Result<Self, MetadataError> {
        let m = closed(
            v,
            "function signature",
            &["parameters", "returns", "type_parameters"],
        )?;
        let mut type_parameters = Vec::new();
        for name in array(field(m, "type_parameters"), "type_parameters")? {
            push(
                &mut type_parameters,
                text(name, "type parameter")?.parse::<Identifier>()?,
            )?;
        }
        let mut parameters = Vec::new();
        for port in array(field(m, "parameters"), "parameters")? {
            push(
                &mut parameters,
                Port::from_value(port, TypeContext::Signature, l)?,
            )?;
        }
        let result = Self {
            type_parameters,
            parameters,
            returns: Port::from_value(field(m, "returns"), TypeContext::Signature, l)?,
        };
        result.validate_declarations()?;
        Ok(result)
    }
}
impl FunctionSignature {
    fn validate_declarations(&self) -> Result<(), MetadataError> {
        let mut declared = BTreeSet::new();
        for name in &self.type_parameters {
            if !declared.insert(name) {
                return Err(MetadataError::DuplicateTypeParameter);
            }
        }
        Ok(())
    }
}
impl Record for Library {
    fn build(&self, l: &Limits) -> Result<Value, MetadataError> {
        let mut b = Builder::new(4, l)?;
        b.text("library_id", &self.library_id)?;
        b.text("version", &self.version)?;
        b.digest("implementation_digest", self.implementation_digest)?;
        b.accounting.text("functions", 1)?;
        b.accounting.collection(self.functions.len(), 1)?;
        let mut functions = Vec::new();
        for (name, signature) in &self.functions {
            b.accounting.text(name.as_str(), 2)?;
            let value = signature.to_value(l)?;
            b.accounting.value(&value, 2)?;
            push(&mut functions, (owned(name.as_str())?, value))?;
        }
        push(
            &mut b.fields,
            (
                owned("functions")?,
                Value::Map(Map::try_from_entries(functions)?),
            ),
        )?;
        b.finish()
    }
    fn parse(v: &Value, l: &Limits) -> Result<Self, MetadataError> {
        let m = closed(
            v,
            "library",
            &[
                "functions",
                "implementation_digest",
                "library_id",
                "version",
            ],
        )?;
        let mut functions = Vec::new();
        for (name, value) in map(field(m, "functions"), "functions")?.iter() {
            push(
                &mut functions,
                (
                    name.parse::<Identifier>()?,
                    FunctionSignature::parse(value, l)?,
                ),
            )?;
        }
        functions.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Ok(Self {
            library_id: string(m, "library_id")?,
            version: string(m, "version")?,
            implementation_digest: digest(m, "implementation_digest")?,
            functions,
        })
    }
}
impl Record for ServerIdentity {
    fn build(&self, l: &Limits) -> Result<Value, MetadataError> {
        let mut b = Builder::new(4, l)?;
        b.text("deployment_id", &self.deployment_id)?;
        b.text("transport", self.transport.as_str())?;
        b.text("implementation_name", &self.implementation_name)?;
        b.text("implementation_version", &self.implementation_version)?;
        b.finish()
    }
    fn parse(v: &Value, _: &Limits) -> Result<Self, MetadataError> {
        let m = closed(
            v,
            "server identity",
            &[
                "deployment_id",
                "implementation_name",
                "implementation_version",
                "transport",
            ],
        )?;
        Ok(Self {
            deployment_id: string(m, "deployment_id")?,
            transport: McpTransport::parse(text(field(m, "transport"), "transport")?)?,
            implementation_name: string(m, "implementation_name")?,
            implementation_version: string(m, "implementation_version")?,
        })
    }
}
impl Record for McpBinding {
    fn build(&self, l: &Limits) -> Result<Value, MetadataError> {
        let mut b = Builder::new(
            if matches!(self.kind, McpBindingKind::Tool { .. }) {
                6
            } else {
                4
            },
            l,
        )?;
        b.child("server", self.server.to_value(l)?)?;
        b.digest("descriptor", self.descriptor)?;
        match &self.kind {
            McpBindingKind::Tool {
                name,
                input_schema,
                output_schema,
            } => {
                b.text("kind", "tool")?;
                b.text("name", name)?;
                b.digest("input_schema", *input_schema)?;
                b.digest("output_schema", *output_schema)?;
            }
            McpBindingKind::Resource { uri } => {
                b.text("kind", "resource")?;
                b.text("uri", uri)?;
            }
            McpBindingKind::Template { uri_template } => {
                b.text("kind", "template")?;
                b.text("uri_template", uri_template)?;
            }
            McpBindingKind::Prompt { name } => {
                b.text("kind", "prompt")?;
                b.text("name", name)?;
            }
        }
        b.finish()
    }
    fn parse(v: &Value, l: &Limits) -> Result<Self, MetadataError> {
        let m = map(v, "MCP binding")?;
        let kind = text(
            m.get("kind").ok_or(MetadataError::MissingField("kind"))?,
            "kind",
        )?;
        let fields: &[&'static str] = match kind {
            "tool" => &[
                "descriptor",
                "input_schema",
                "kind",
                "name",
                "output_schema",
                "server",
            ],
            "resource" => &["descriptor", "kind", "server", "uri"],
            "template" => &["descriptor", "kind", "server", "uri_template"],
            "prompt" => &["descriptor", "kind", "name", "server"],
            _ => return Err(MetadataError::UnknownBindingKind),
        };
        closed(v, "MCP binding", fields)?;
        let server = ServerIdentity::parse(field(m, "server"), l)?;
        let descriptor = digest(m, "descriptor")?;
        let kind = match kind {
            "tool" => McpBindingKind::Tool {
                name: string(m, "name")?,
                input_schema: digest(m, "input_schema")?,
                output_schema: digest(m, "output_schema")?,
            },
            "resource" => McpBindingKind::Resource {
                uri: string(m, "uri")?,
            },
            "template" => McpBindingKind::Template {
                uri_template: string(m, "uri_template")?,
            },
            "prompt" => McpBindingKind::Prompt {
                name: string(m, "name")?,
            },
            _ => unreachable!("kind checked above"),
        };
        Ok(Self {
            server,
            descriptor,
            kind,
        })
    }
}

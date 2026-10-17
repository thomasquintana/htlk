//! Canonical, source-independent analysis reports.
use crate::{ExpressionSite, LinkageError, ScopeUse};
use htlk_executable::{
    CanonicalDocument, Expression, ExpressionKind as E, JsonPointer, cbor::Limits, digest::Digest,
};

/// A canonical document location and the actual scope use, when applicable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisLocation {
    /// RFC 6901 pointer into the canonical document (external JSON remains bytes).
    pub pointer: JsonPointer,
    /// Actual root/task/loop use that supplied this context.
    pub scope_use: Option<ScopeUse>,
}
impl AnalysisLocation {
    pub(crate) fn stored_size(&self) -> usize {
        size_of::<Self>()
            .saturating_add(
                self.pointer
                    .tokens()
                    .iter()
                    .map(|s| size_of::<String>().saturating_add(s.len()))
                    .sum::<usize>(),
            )
            .saturating_add(match &self.scope_use {
                Some(ScopeUse::Node { node, .. }) => node.as_str().len(),
                _ => 0,
            })
    }
}
/// Severity of a semantic report entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    /// Analysis cannot succeed.
    Error,
    /// An admitted graph requires the reported runtime check.
    Warning,
}
/// Deterministic compiler-facing report; source spans can be joined by canonical path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisDiagnostic {
    /// Stable specification diagnostic family.
    pub code: &'static str,
    /// Whether this prevents admission.
    pub severity: DiagnosticSeverity,
    /// Content-independent explanation.
    pub message: &'static str,
    /// Primary canonical location.
    pub location: AnalysisLocation,
    /// Other records participating in this failure or warning.
    pub related_locations: Vec<AnalysisLocation>,
}
impl AnalysisDiagnostic {
    pub(crate) fn check_limits(&self, limits: &Limits) -> Result<(), LinkageError> {
        let mut bytes = size_of::<Self>();
        for loc in std::iter::once(&self.location).chain(&self.related_locations) {
            location(&loc.pointer.to_string(), loc.scope_use.clone(), limits)?;
            bytes = bytes.saturating_add(loc.stored_size());
            if bytes > limits.max_document_bytes {
                return Err(LinkageError::LimitExceeded {
                    limit: htlk_executable::cbor::LimitKind::DocumentBytes,
                    maximum: limits.max_document_bytes,
                });
            }
        }
        Ok(())
    }
}
/// Linkage failure retaining the original category and its canonical origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkageDiagnostic {
    /// Location of the declaration or use that failed.
    pub location: AnalysisLocation,
    /// Other declarations participating in an interface/identity conflict.
    pub related_locations: Vec<AnalysisLocation>,
    /// Original structured cause.
    pub error: LinkageError,
}
impl std::fmt::Display for LinkageDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "linkage at {:?}: {}", self.location, self.error)
    }
}
impl std::error::Error for LinkageDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
pub(crate) fn escape(text: &str) -> String {
    text.replace('~', "~0").replace('/', "~1")
}
pub(crate) fn location(
    path: &str,
    scope_use: Option<ScopeUse>,
    limits: &Limits,
) -> Result<AnalysisLocation, LinkageError> {
    let pointer = JsonPointer::new(path, limits).map_err(|_| LinkageError::LimitExceeded {
        limit: htlk_executable::cbor::LimitKind::DocumentBytes,
        maximum: limits.max_document_bytes,
    })?;
    Ok(AnalysisLocation { pointer, scope_use })
}
pub(crate) fn child_path(expression: &Expression, base: &str, child: usize) -> Option<String> {
    Some(match expression.kind() {
        E::Get { .. } | E::Not(_) if child == 0 => format!("{base}/1"),
        E::Binary { .. } if child < 2 => format!("{base}/{}", child + 1),
        E::List(items) if child < items.len() => format!("{base}/1/{child}"),
        E::Call { arguments, .. } if child < arguments.len() => format!("{base}/2/{child}"),
        E::Record(fields) if child < fields.len() => format!("{base}/1/{child}/1"),
        E::Render { arguments, .. } => {
            format!("{base}/2/{}", escape(arguments.get(child)?.0.as_str()))
        }
        _ => return None,
    })
}
pub(crate) fn expression_location(
    document: &CanonicalDocument,
    scope: Digest,
    scope_use: &ScopeUse,
    site: &ExpressionSite,
    children: &[usize],
    limits: &Limits,
) -> Result<AnalysisLocation, LinkageError> {
    let f = document.fields();
    let s = f
        .scopes
        .get(&scope)
        .ok_or(LinkageError::MissingRecord("scope"))?
        .fields();
    let base = format!("/scopes/{scope}");
    let node_site = |id: &htlk_executable::Identifier| {
        s.nodes
            .iter()
            .position(|n| n.id() == id)
            .ok_or(LinkageError::MissingRecord("node"))
    };
    let (mut path, mut expression) = match site {
        ExpressionSite::Preconditions => (format!("{base}/preconditions"), Some(&s.preconditions)),
        ExpressionSite::Postconditions => {
            (format!("{base}/postconditions"), Some(&s.postconditions))
        }
        ExpressionSite::NodeGuard(id)
        | ExpressionSite::NodePreconditions(id)
        | ExpressionSite::NodePostconditions(id)
        | ExpressionSite::Eval(id) => {
            let index = node_site(id)?;
            let n = s.nodes[index].fields();
            let (suffix, e) = match site {
                ExpressionSite::NodeGuard(_) => ("guard", &n.guard),
                ExpressionSite::NodePreconditions(_) => ("preconditions", &n.preconditions),
                ExpressionSite::NodePostconditions(_) => ("postconditions", &n.postconditions),
                _ => {
                    let htlk_executable::Operation::Eval(e) = &n.operation else {
                        return Err(LinkageError::MissingRecord("eval"));
                    };
                    ("operation/1", e)
                }
            };
            (format!("{base}/nodes/{index}/{suffix}"), Some(e))
        }
        ExpressionSite::EdgeGuard(id) | ExpressionSite::EdgeBoundary(id) => {
            let index = s
                .edges
                .iter()
                .position(|e| e.id() == id)
                .ok_or(LinkageError::MissingRecord("edge"))?;
            if matches!(site, ExpressionSite::EdgeGuard(_)) {
                (
                    format!("{base}/edges/{index}/guard"),
                    Some(s.edges[index].guard()),
                )
            } else {
                (format!("{base}/edges/{index}"), None)
            }
        }
        ExpressionSite::Until => {
            let ScopeUse::Node { scope: owner, node } = scope_use else {
                return Err(LinkageError::ScopeInterfaceMismatch);
            };
            let owner_fields = f
                .scopes
                .get(owner)
                .ok_or(LinkageError::MissingRecord("scope"))?
                .fields();
            let index = owner_fields
                .nodes
                .iter()
                .position(|n| n.id() == node)
                .ok_or(LinkageError::MissingRecord("node"))?;
            let htlk_executable::Operation::Loop { until, .. } =
                &owner_fields.nodes[index].fields().operation
            else {
                return Err(LinkageError::ScopeInterfaceMismatch);
            };
            (
                format!("/scopes/{owner}/nodes/{index}/operation/3"),
                Some(until),
            )
        }
    };
    for &child in children {
        let e = expression.ok_or(LinkageError::MissingRecord("expression"))?;
        path =
            child_path(e, &path, child).ok_or(LinkageError::MissingRecord("expression child"))?;
        expression = match e.kind() {
            E::Get { value, .. } | E::Not(value) => Some(value),
            E::Binary { left, right, .. } => Some(if child == 0 { left } else { right }),
            E::List(items)
            | E::Call {
                arguments: items, ..
            } => items.get(child),
            E::Record(fields) => fields.get(child).map(|(_, e)| e),
            E::Render { arguments, .. } => arguments.get(child).map(|(_, e)| e),
            _ => None,
        };
    }
    location(&path, Some(scope_use.clone()), limits)
}

pub(crate) fn graph_location(
    document: &CanonicalDocument,
    error: &crate::GraphVerificationError,
    limits: &Limits,
) -> Result<(AnalysisLocation, Vec<AnalysisLocation>), LinkageError> {
    use crate::{ScopeVerificationErrorKind as K, WaitVertex as V};
    let Some(scope) = error.scope else {
        return Ok((location("", error.scope_use.clone(), limits)?, Vec::new()));
    };
    let base = format!("/scopes/{scope}");
    let fields = document
        .fields()
        .scopes
        .get(&scope)
        .ok_or(LinkageError::MissingRecord("scope"))?
        .fields();
    let node_index = |id: &htlk_executable::Identifier| {
        fields
            .nodes
            .iter()
            .position(|n| n.id() == id)
            .ok_or(LinkageError::MissingRecord("node"))
    };
    let destination = match &error.cause.kind {
        K::MissingCandidate(v) | K::UnconditionalWriters(v) => Some(v),
        _ => None,
    };
    let path = match (&error.cause.kind, destination) {
        (_, Some(V::Input { node, port })) => {
            format!("{base}/nodes/{}/inputs/{port}", node_index(node)?)
        }
        (_, Some(V::Output(port))) => format!("{base}/outputs/{port}"),
        (_, Some(V::Next(port))) => format!("{base}/carried/{port}"),
        (K::Unobservable(node), _) => format!("{base}/nodes/{}", node_index(node)?),
        _ => base.clone(),
    };
    let mut related = Vec::new();
    if let K::UnconditionalWriters(_) = &error.cause.kind {
        let mut bytes = 0usize;
        for (index, edge) in fields.edges.iter().enumerate() {
            use htlk_executable::EdgeDestination as D;
            let vertex = match edge.destination() {
                D::Input { node, port } => V::Input {
                    node: node.clone(),
                    port: port.clone(),
                },
                D::Output(port) => V::Output(port.clone()),
                D::Next(port) => V::Next(port.clone()),
            };
            if Some(&vertex) == destination {
                let path = format!("{base}/edges/{index}");
                bytes = bytes.saturating_add(path.len() + size_of::<AnalysisLocation>());
                if bytes > limits.max_document_bytes {
                    return Err(LinkageError::LimitExceeded {
                        limit: htlk_executable::cbor::LimitKind::DocumentBytes,
                        maximum: limits.max_document_bytes,
                    });
                }
                related.push(location(&path, error.scope_use.clone(), limits)?);
            }
        }
    }
    Ok((location(&path, error.scope_use.clone(), limits)?, related))
}

impl LinkageError {
    /// Specification diagnostic family for this semantic failure.
    pub fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::MissingRecord("library" | "function") => "C_LIBRARY",
            Self::MissingRecord("template") => "C_PROMPT",
            Self::MissingRecord("binding" | "descriptor document") => "C_MCP",
            Self::MissingRecord(_) => "C_NAME",
            Self::UnreachableRecord(_) => "C_UNUSED_NODE",
            Self::ScopeCycle => "C_DEPENDENCY_CYCLE",
            Self::Graph(htlk_executable::GraphRecordError::InvalidScopeRole) => "C_LOOP",
            Self::ScopeInterfaceMismatch | Self::Graph(_) => "C_SCOPE",
            Self::FunctionArity | Self::Metadata(_) => "C_LIBRARY",
            Self::TemplateArguments => "C_PROMPT",
            Self::Type(_) | Self::UnreachedSchemaType => "C_TYPE",
            Self::Policy(_) | Self::StructuralLimitExceeded { .. } => "C_POLICY",
            Self::SchemaDiagnostic(_)
            | Self::NativeSchema(_)
            | Self::Schema(_)
            | Self::SchemaRootMismatch => "C_SCHEMA",
            Self::Mcp(_)
            | Self::ConflictingMcpSelection
            | Self::InvalidDescriptor(_)
            | Self::InvalidUriTemplate { .. }
            | Self::ToolSchemaMismatch(_)
            | Self::McpInterfaceMismatch => "C_MCP",
            Self::Expression(_) => "C_SCOPE",
            _ => "C_SERIALIZATION",
        }
    }
}

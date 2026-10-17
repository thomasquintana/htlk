//! Generated public interfaces backed by checked implementation bodies.
use crate::executable::digest::Digest;
use crate::{
    Compiler, Diagnostic, MCPCatalogInputs,
    ast::DeclKind,
    bundle::{ModuleRef, SourceBundle},
    diagnostic::{Result, fail},
    executable::*,
};
use std::collections::BTreeSet;

/// The three exportable source declaration kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportKind {
    /// Structural alias.
    Type,
    /// Local prompt template.
    Prompt,
    /// Reusable graph implementation.
    Task,
}
/// A discoverable exported name and kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportIndexEntry {
    /// Exact exported name.
    pub name: String,
    /// Declaration kind.
    pub kind: ExportKind,
}
/// Verified implementation-derived summary; contains no prompt/task body.
#[derive(Clone, Debug)]
pub enum ExportSummary {
    /// Resolved structural type alias.
    Type {
        /// Exact name.
        name: String,
        /// Canonical resolved type.
        value_type: ValueType,
    },
    /// Local prompt interface.
    Prompt {
        /// Exact name.
        name: String,
        /// Resolved parameter declarations.
        parameters: Vec<(Identifier, Port)>,
        /// Complete template identity.
        template_digest: Digest,
    },
    /// Reusable task interface and implementation identity.
    Task {
        /// Exact name.
        name: String,
        /// Authored description, default empty.
        description: String,
        /// Public inputs.
        inputs: PortTable,
        /// Public outputs.
        outputs: PortTable,
        /// Normalized implementation identity.
        scope_digest: Digest,
        /// Input contract.
        preconditions: Expression,
        /// Completion contract.
        postconditions: Expression,
        /// Local inherited ceilings.
        limits: ExecutionLimits,
    },
}
/// Public module inspection tied to exact source/profile identities.
#[derive(Clone, Debug)]
pub struct ModuleInspectionSuccess {
    /// Always 0.1.
    pub interface_version: &'static str,
    /// Exact inspected snapshot/module.
    pub module: ModuleRef,
    /// Exact compiler profile used for checking.
    pub profile: ExecutionProfile,
    /// Complete name-sorted export index.
    pub available_exports: Vec<ExportIndexEntry>,
    /// Requested summaries, sorted by name.
    pub exports: Vec<ExportSummary>,
    /// Semantic warnings.
    pub diagnostics: Vec<Diagnostic>,
}
impl Compiler {
    /// Checks the selected module/import closure before filtering public summaries.
    /// An empty symbols list requests only the complete export index.
    ///
    /// # Errors
    /// Rejects invalid implementations, duplicate/private/unknown requested symbols,
    /// invalid snapshots, or resource limits. No partial verified report is returned.
    pub fn inspect_module(
        &self,
        bundle: &SourceBundle,
        module: &ModuleRef,
        symbols: &[String],
        catalogs: &MCPCatalogInputs,
    ) -> Result<ModuleInspectionSuccess> {
        let (mut session, root) = crate::modules::session(self, bundle, module, catalogs, true)?;
        session.check_all()?;
        let requested = symbols.iter().collect::<BTreeSet<_>>();
        if requested.len() != symbols.len() {
            return Err(fail("C_DUPLICATE", "duplicate requested interface symbol"));
        }
        let mut available_exports = vec![];
        let mut exports = vec![];
        let declarations = session.symbols[root].values().cloned().collect::<Vec<_>>();
        for d in declarations {
            if !d.exported {
                continue;
            }
            let kind = match &d.kind {
                DeclKind::Type(_) => ExportKind::Type,
                DeclKind::Prompt(..) => ExportKind::Prompt,
                DeclKind::Task(_) => ExportKind::Task,
                _ => unreachable!("parser restricts exports"),
            };
            available_exports.push(ExportIndexEntry {
                name: d.name.clone(),
                kind,
            });
            if !requested.contains(&d.name) {
                continue;
            }
            exports.push(match &d.kind {
                DeclKind::Type(t) => ExportSummary::Type {
                    name: d.name.clone(),
                    value_type: session.ty(root, t)?,
                },
                DeclKind::Prompt(..) => {
                    let id = session.prompt(root, &d.name)?;
                    ExportSummary::Prompt {
                        name: d.name.clone(),
                        parameters: session.pool.templates[&id].parameters().to_vec(),
                        template_digest: id,
                    }
                }
                DeclKind::Task(b) => {
                    let id = session.scope_decl(root, &d.name)?;
                    let f = session.pool.scopes[&id].fields();
                    ExportSummary::Task {
                        name: d.name.clone(),
                        description: b.description.clone().unwrap_or_default(),
                        inputs: f.inputs.clone(),
                        outputs: f.outputs.clone(),
                        scope_digest: id,
                        preconditions: f.preconditions.clone(),
                        postconditions: f.postconditions.clone(),
                        limits: f.limits.clone(),
                    }
                }
                _ => unreachable!(),
            });
        }
        if requested
            .iter()
            .any(|n| !available_exports.iter().any(|e| &e.name == *n))
        {
            return Err(fail(
                "C_VISIBILITY",
                "requested interface symbol is private or unknown",
            ));
        }
        let mut diagnostics = session
            .diagnostics
            .iter()
            .cloned()
            .map(|mut d| {
                d.location = session.source_location(&d.location);
                d.related_locations = d
                    .related_locations
                    .iter()
                    .map(|l| session.source_location(l))
                    .collect();
                d
            })
            .collect::<Vec<_>>();
        crate::diagnostic::sort(&mut diagnostics);
        let l = &self.config.limits;
        let mut report_bytes = available_exports.iter().fold(0usize, |n, e| {
            n.saturating_add(e.name.len())
                .saturating_add(size_of::<ExportIndexEntry>())
        });
        for summary in &exports {
            let bytes = match summary {
                ExportSummary::Type { name, value_type } => name.len().saturating_add(
                    value_type
                        .encode(TypeContext::Value, l)
                        .map_err(|e| fail("C_SERIALIZATION", e))?
                        .len(),
                ),
                ExportSummary::Prompt {
                    name, parameters, ..
                } => {
                    let mut bytes = name.len();
                    for (name, p) in parameters {
                        bytes = bytes.saturating_add(name.as_str().len()).saturating_add(
                            p.encode(TypeContext::Value, l)
                                .map_err(|e| fail("C_SERIALIZATION", e))?
                                .len(),
                        );
                    }
                    bytes
                }
                ExportSummary::Task {
                    name,
                    description,
                    inputs,
                    outputs,
                    preconditions,
                    postconditions,
                    limits,
                    ..
                } => name
                    .len()
                    .saturating_add(description.len())
                    .saturating_add(
                        inputs
                            .encode(l)
                            .map_err(|e| fail("C_SERIALIZATION", e))?
                            .len(),
                    )
                    .saturating_add(
                        outputs
                            .encode(l)
                            .map_err(|e| fail("C_SERIALIZATION", e))?
                            .len(),
                    )
                    .saturating_add(
                        preconditions
                            .encode(l)
                            .map_err(|e| fail("C_SERIALIZATION", e))?
                            .len(),
                    )
                    .saturating_add(
                        postconditions
                            .encode(l)
                            .map_err(|e| fail("C_SERIALIZATION", e))?
                            .len(),
                    )
                    .saturating_add(
                        limits
                            .encode(l)
                            .map_err(|e| fail("C_SERIALIZATION", e))?
                            .len(),
                    ),
            };
            report_bytes = report_bytes
                .saturating_add(bytes)
                .saturating_add(size_of::<ExportSummary>());
        }
        report_bytes = diagnostics.iter().fold(report_bytes, |n, d| {
            n.saturating_add(crate::diagnostic::diagnostic_size(d))
        });
        if report_bytes > self.config.frontend.max_report_bytes {
            return Err(fail("C_SOURCE_LIMIT", "interface report limit exceeded"));
        }
        Ok(ModuleInspectionSuccess {
            interface_version: "0.1",
            module: module.clone(),
            profile: self.config.profile.clone(),
            available_exports,
            exports,
            diagnostics,
        })
    }
}

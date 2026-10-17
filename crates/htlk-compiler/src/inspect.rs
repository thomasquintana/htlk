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
        diagnostics.sort_by(|a, b| a.location.cmp(&b.location).then(a.code.cmp(&b.code)));
        diagnostics.dedup();
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

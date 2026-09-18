//! Structural bounds computed over definitions, never expanded invocations.

use std::collections::BTreeMap;

use crate::digest::Digest;
use crate::{DocumentError, DocumentFields, Operation, PolicyFields};

/// The policy ceiling violated by structural analysis; distinct from codec limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructuralLimit {
    /// Nested scope depth, with the root scope counted as one.
    ScopeDepth,
    /// Maximum possible node invocation occurrences in a fresh execution.
    ExpandedNodes,
}

/// Derived static bounds for the root graph, checked against its pinned policy.
/// This is not serialized, an execution counter, or a reservation of runtime storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StructuralSummary {
    scope_depth: u64,
    expanded_nodes: u64,
}
impl StructuralSummary {
    /// Longest root-to-body scope chain, counting root and each body once.
    pub const fn scope_depth(self) -> u64 {
        self.scope_depth
    }

    /// Potential node invocations, including wrappers, reused bodies, every
    /// guarded branch, and loop iteration multipliers. The root is not a node.
    pub const fn expanded_nodes(self) -> u64 {
        self.expanded_nodes
    }
}

struct Pending {
    children: usize,
    summary: StructuralSummary,
}

/// The caller has bounded the whole document and checked its definition closure.
/// Reverse dependencies retain one entry per use, including repeated use of the
/// same definition. Leaf-to-root propagation costs O(definitions + uses) map
/// operations and storage, independently of iteration counts/expanded node totals.
pub(crate) fn analyze(
    f: &DocumentFields,
    policy: &PolicyFields,
) -> Result<StructuralSummary, DocumentError> {
    let mut pending = BTreeMap::new();
    let mut parents: BTreeMap<Digest, Vec<(Digest, u64)>> = BTreeMap::new();
    for (id, scope) in &f.scopes {
        let nodes = &scope.fields().nodes;
        let count = u64::try_from(nodes.len()).map_err(|_| {
            exceeded(
                StructuralLimit::ExpandedNodes,
                policy.maximum_expanded_nodes,
            )
        })?;
        bound(
            count,
            policy.maximum_expanded_nodes,
            StructuralLimit::ExpandedNodes,
        )?;
        let mut children = 0;
        for node in nodes {
            let (child, multiplier) = match &node.fields().operation {
                Operation::Scope(child) => (*child, 1),
                Operation::Loop {
                    body,
                    max_iterations,
                    ..
                } => (*body, *max_iterations),
                _ => continue,
            };
            if !f.scopes.contains_key(&child) {
                return Err(DocumentError::MissingRecord("scope"));
            }
            children += 1;
            let uses = parents.entry(child).or_default();
            uses.try_reserve(1)
                .map_err(|_| DocumentError::AllocationFailed)?;
            uses.push((*id, multiplier));
        }
        pending.insert(
            *id,
            Pending {
                children,
                summary: StructuralSummary {
                    scope_depth: 1,
                    expanded_nodes: count,
                },
            },
        );
    }
    let mut ready = Vec::new();
    ready
        .try_reserve_exact(pending.len())
        .map_err(|_| DocumentError::AllocationFailed)?;
    ready.extend(
        pending
            .iter()
            .filter_map(|(id, p)| (p.children == 0).then_some(*id)),
    );
    let mut completed = 0;
    while let Some(id) = ready.pop() {
        completed += 1;
        let child = pending[&id].summary;
        if let Some(uses) = parents.get(&id) {
            for (parent_id, multiplier) in uses {
                let parent = pending.get_mut(parent_id).expect("parent inserted");
                let depth = child.scope_depth.checked_add(1).ok_or_else(|| {
                    exceeded(StructuralLimit::ScopeDepth, policy.maximum_scope_depth)
                })?;
                bound(
                    depth,
                    policy.maximum_scope_depth,
                    StructuralLimit::ScopeDepth,
                )?;
                parent.summary.scope_depth = parent.summary.scope_depth.max(depth);
                let nodes = child
                    .expanded_nodes
                    .checked_mul(*multiplier)
                    .and_then(|n| parent.summary.expanded_nodes.checked_add(n))
                    .ok_or_else(|| {
                        exceeded(
                            StructuralLimit::ExpandedNodes,
                            policy.maximum_expanded_nodes,
                        )
                    })?;
                bound(
                    nodes,
                    policy.maximum_expanded_nodes,
                    StructuralLimit::ExpandedNodes,
                )?;
                parent.summary.expanded_nodes = nodes;
                parent.children -= 1;
                if parent.children == 0 {
                    ready.push(*parent_id);
                }
            }
        }
    }
    if completed != pending.len() {
        return Err(DocumentError::ScopeCycle);
    }
    let summary = pending
        .get(&f.root_scope)
        .ok_or(DocumentError::MissingRecord("scope"))?
        .summary;
    bound(
        summary.scope_depth,
        policy.maximum_scope_depth,
        StructuralLimit::ScopeDepth,
    )?;
    Ok(summary)
}

fn exceeded(limit: StructuralLimit, maximum: u64) -> DocumentError {
    DocumentError::StructuralLimitExceeded { limit, maximum }
}
fn bound(value: u64, maximum: u64, limit: StructuralLimit) -> Result<(), DocumentError> {
    if value > maximum {
        Err(exceeded(limit, maximum))
    } else {
        Ok(())
    }
}

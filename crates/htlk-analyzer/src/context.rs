//! Local name membership and use-context legality, independent of wire syntax.
use htlk_executable::*;
use std::collections::BTreeMap;

/// Checks contextual reference/outcome availability without resolving declarations
/// or inferring types. Useful for runtime's lower-level evaluator integration.
///
/// # Errors
/// Returns bounded representation failures or a located forbidden-reference error.
pub fn check_expression_context(
    expression: &Expression,
    context: ExpressionContext,
    limits: &cbor::Limits,
) -> Result<(), crate::ExpressionDiagnostic> {
    expression
        .to_value(context, limits)
        .map_err(|e| crate::ExpressionDiagnostic {
            expression_path: vec![],
            error: e.into(),
        })?;
    fn walk(
        e: &Expression,
        c: ExpressionContext,
        path: &mut Vec<usize>,
    ) -> Result<(), crate::ExpressionDiagnostic> {
        let result = match e.kind() {
            ExpressionKind::Ref { source, .. } => reference(source, c),
            ExpressionKind::Status(_) | ExpressionKind::Error(_) => outcome(c),
            _ => Ok(()),
        };
        result.map_err(|e| crate::ExpressionDiagnostic {
            expression_path: path.clone(),
            error: e.into(),
        })?;
        let mut child = |index, e: &Expression| {
            path.try_reserve(1)
                .map_err(|_| crate::ExpressionDiagnostic {
                    expression_path: path.clone(),
                    error: crate::ExpressionTypeError::AllocationFailed,
                })?;
            path.push(index);
            let result = walk(e, c, path);
            path.pop();
            result
        };
        match e.kind() {
            ExpressionKind::Get { value, .. } | ExpressionKind::Not(value) => child(0, value)?,
            ExpressionKind::Binary { left, right, .. } => {
                child(0, left)?;
                child(1, right)?;
            }
            ExpressionKind::List(items)
            | ExpressionKind::Call {
                arguments: items, ..
            } => {
                for (i, e) in items.iter().enumerate() {
                    child(i, e)?;
                }
            }
            ExpressionKind::Record(fields) => {
                for (i, (_, e)) in fields.iter().enumerate() {
                    child(i, e)?;
                }
            }
            ExpressionKind::Render { arguments, .. } => {
                for (i, (_, e)) in arguments.iter().enumerate() {
                    child(i, e)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    walk(expression, context, &mut Vec::new())
}

pub(crate) fn scope(scope: &Scope, c: ScopeContext) -> Result<(), GraphRecordError> {
    let f = scope.fields();
    if c == ScopeContext::Ordinary && !f.carried.is_empty() {
        return Err(GraphRecordError::InvalidScopeRole);
    }
    if c == ScopeContext::LoopBody
        && (f.limits != ExecutionLimits::new()
            || !truth(&f.preconditions)
            || !truth(&f.postconditions))
    {
        return Err(GraphRecordError::InvalidScopeRole);
    }
    let nodes: BTreeMap<_, _> = f
        .nodes
        .iter()
        .map(|n| (n.id().as_str(), n.fields()))
        .collect();
    for node in &f.nodes {
        let n = node.fields();
        if let Operation::Loop { initializers, .. } = &n.operation {
            for (_, input) in initializers {
                require_port(&n.inputs, input, "loop initializer input")?;
            }
        }
    }
    for edge in &f.edges {
        match edge.source() {
            EdgeSource::Input(n) => require_port(&f.inputs, n, "scope input")?,
            EdgeSource::Carried(n) => {
                if c != ScopeContext::LoopBody {
                    return Err(GraphRecordError::InvalidScopeRole);
                }
                require_port(&f.carried, n, "carried source")?;
            }
            EdgeSource::Output { node, port } => require_port(
                &nodes
                    .get(node.as_str())
                    .ok_or(GraphRecordError::UnknownEndpoint("source node"))?
                    .outputs,
                port,
                "source output",
            )?,
        }
        match edge.destination() {
            EdgeDestination::Output(n) => require_port(&f.outputs, n, "scope output")?,
            EdgeDestination::Next(n) => {
                if c != ScopeContext::LoopBody {
                    return Err(GraphRecordError::InvalidScopeRole);
                }
                require_port(&f.carried, n, "next destination")?;
            }
            EdgeDestination::Input { node, port } => require_port(
                &nodes
                    .get(node.as_str())
                    .ok_or(GraphRecordError::UnknownEndpoint("destination node"))?
                    .inputs,
                port,
                "destination input",
            )?,
        }
    }
    Ok(())
}
fn truth(e: &Expression) -> bool {
    matches!(
        e.kind(),
        ExpressionKind::Literal(ScalarLiteral::Boolean(true))
    )
}
fn require_port(
    table: &PortTable,
    name: &Identifier,
    site: &'static str,
) -> Result<(), GraphRecordError> {
    if table.get(name.as_str()).is_some() {
        Ok(())
    } else {
        Err(GraphRecordError::UnknownEndpoint(site))
    }
}

pub(crate) fn reference(r: &ValueReference, c: ExpressionContext) -> Result<(), ExpressionError> {
    use ExpressionContext as C;
    let (allowed, label) = match r {
        ValueReference::Input(_) => (true, "input"),
        ValueReference::Output { .. } => (matches!(c, C::Guard { .. } | C::LoopUntil), "output"),
        ValueReference::ScopeOutput(_) => (
            matches!(
                c,
                C::PrimitivePostconditions
                    | C::ScopePostconditions
                    | C::WrapperPostconditions
                    | C::LoopUntil
                    | C::LoopPostconditions
            ),
            "scope_output",
        ),
        ValueReference::Carried(_) => (
            matches!(c, C::Guard { loop_body: true } | C::LoopUntil),
            "carried",
        ),
        ValueReference::Next(_) => (matches!(c, C::LoopUntil), "next"),
    };
    if allowed {
        Ok(())
    } else {
        Err(ExpressionError::ForbiddenReference(label))
    }
}
pub(crate) fn outcome(c: ExpressionContext) -> Result<(), ExpressionError> {
    if matches!(
        c,
        ExpressionContext::Guard { .. }
            | ExpressionContext::ScopePostconditions
            | ExpressionContext::LoopUntil
    ) {
        Ok(())
    } else {
        Err(ExpressionError::ForbiddenReference("node outcome"))
    }
}

/// Checks bounded signature representation and resolves its declared variables.
///
/// # Errors
/// Returns metadata limits or an undeclared generic variable.
pub fn check_function_signature(
    value: &FunctionSignature,
    limits: &cbor::Limits,
) -> Result<(), MetadataError> {
    value.to_value(limits)?;
    signature(value)
}

/// Checks bounded template representation and exact slot/parameter name coverage.
///
/// # Errors
/// Returns representation limits, allocation failures or a parameter mismatch.
pub fn check_prompt_template(
    value: &PromptTemplate,
    limits: &cbor::Limits,
) -> Result<(), ExpressionError> {
    value.to_value(limits)?;
    template(value)
}
pub(crate) fn template(value: &PromptTemplate) -> Result<(), ExpressionError> {
    let mut slots = Vec::new();
    for part in value.parts() {
        if let TemplatePart::Slot(name) = part {
            slots
                .try_reserve(1)
                .map_err(|_| ExpressionError::AllocationFailed)?;
            slots.push(name);
        }
    }
    slots.sort_unstable();
    slots.dedup();
    if slots.len() != value.parameters().len()
        || slots
            .iter()
            .zip(value.parameters())
            .any(|(slot, (name, _))| *slot != name)
    {
        return Err(ExpressionError::TemplateParameterMismatch);
    }
    Ok(())
}

pub(crate) fn signature(signature: &FunctionSignature) -> Result<(), MetadataError> {
    fn variables(ty: &ValueType, declared: &[Identifier]) -> Result<(), MetadataError> {
        match ty.kind() {
            ValueTypeKind::Var(n) => {
                if !declared.contains(n) {
                    return Err(MetadataError::UndeclaredTypeVariable);
                }
            }
            ValueTypeKind::List(t) | ValueTypeKind::Map(t) => variables(t, declared)?,
            ValueTypeKind::Record(fields) => {
                for (_, p) in fields {
                    variables(p.value_type(), declared)?;
                }
            }
            ValueTypeKind::Union(types) => {
                for t in types {
                    variables(t, declared)?;
                }
            }
            ValueTypeKind::Function {
                parameters,
                returns,
            } => {
                for p in parameters.iter().chain(std::iter::once(returns.as_ref())) {
                    variables(p.value_type(), declared)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    for p in signature
        .parameters()
        .iter()
        .chain(std::iter::once(signature.returns()))
    {
        variables(p.value_type(), signature.type_parameters())?;
    }
    Ok(())
}

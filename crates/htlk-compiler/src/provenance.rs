//! Map normalized expression children to their original operands.
use crate::{
    SourceMapEntry,
    ast::{Expr, ExprKind},
    executable::{Expression, ExpressionKind},
};

pub(crate) fn expression(path: &str, e: &Expression, source: &Expr, out: &mut Vec<SourceMapEntry>) {
    out.push(SourceMapEntry {
        canonical_path: path.into(),
        span: source.span.clone(),
        declaration_name: None,
    });
    match (e.kind(), &source.value) {
        (ExpressionKind::Binary { left, right, .. }, ExprKind::Binary(_, a, b)) => {
            expression(&format!("{path}/1"), left, a, out);
            expression(&format!("{path}/2"), right, b, out);
        }
        (ExpressionKind::Not(value), ExprKind::Not(s)) => {
            expression(&format!("{path}/1"), value, s, out)
        }
        (ExpressionKind::Get { value, .. }, ExprKind::Get(..)) => {
            let mut base = source;
            while let ExprKind::Get(s, _) = &base.value {
                base = s;
            }
            expression(&format!("{path}/1"), value, base, out);
        }
        (ExpressionKind::List(values), ExprKind::List(sources)) => {
            for (i, (e, s)) in values.iter().zip(sources).enumerate() {
                expression(&format!("{path}/1/{i}"), e, s, out)
            }
        }
        (ExpressionKind::Record(fields), ExprKind::Record(sources)) => {
            for (i, (name, e)) in fields.iter().enumerate() {
                if let Some((_, s)) = sources.iter().find(|(n, _)| n == name) {
                    expression(&format!("{path}/1/{i}/1"), e, s, out)
                }
            }
        }
        (ExpressionKind::Call { arguments, .. }, ExprKind::Call(_, sources)) => {
            for (i, (e, s)) in arguments.iter().zip(sources).enumerate() {
                expression(&format!("{path}/2/{i}"), e, s, out)
            }
        }
        (ExpressionKind::Render { arguments, .. }, ExprKind::Call(_, sources)) => {
            if let Some(Expr {
                value: ExprKind::Record(sources),
                ..
            }) = sources.get(1)
            {
                for (name, e) in arguments {
                    if let Some((_, s)) = sources.iter().find(|(n, _)| n == name.as_str()) {
                        let key = name.as_str().replace('~', "~0").replace('/', "~1");
                        expression(&format!("{path}/2/{key}"), e, s, out);
                    }
                }
            }
        }
        _ => {}
    }
}

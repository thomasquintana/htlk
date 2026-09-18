//! Runtime rejects malformed template bindings from both frames and custom contexts.
use htlk_executable::{cbor::Limits, digest::Digest, *};
use htlk_rt::*;

struct Custom(PromptTemplate);
impl EvaluationContext for Custom {
    fn resolve(
        &self,
        _: &ValueReference,
        _: &[PathStep],
        _: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        Err(EvaluationError::UnknownReference)
    }
    fn outcome(&self, _: &Identifier) -> Option<&EvaluationOutcome> {
        None
    }
    fn template(&self, _: &Digest) -> Option<&PromptTemplate> {
        Some(&self.0)
    }
}
#[test]
fn malformed_templates_fail_before_registration_or_rendering() {
    let limits = Limits::default();
    let template = PromptTemplate::new(
        vec![],
        vec![TemplatePart::Slot("missing".parse().unwrap())],
        &limits,
    )
    .unwrap();
    let digest = template.digest(&limits).unwrap();
    let expected = EvaluationError::Expression(ExpressionError::TemplateParameterMismatch);
    let mut frame = EvaluationFrame::default();
    assert_eq!(
        frame.insert_template(template.clone(), &limits),
        Err(expected.clone())
    );
    assert!(frame.template(&digest).is_none());
    let expression = Expression::new(
        ExpressionKind::Render {
            template: digest,
            arguments: vec![],
        },
        ExpressionContext::Eval,
        &limits,
    )
    .unwrap();
    let policy = EvaluatorLimits {
        max_expression_depth: 64,
        max_value_bytes: 1024,
        max_collection_visits: 1000,
        max_regex_bytes: 1024,
        max_regex_compiled_bytes: 4096,
        max_output_bytes: 1024,
        max_steps: 10000,
    };
    assert_eq!(
        evaluate(
            &expression,
            ExpressionContext::Eval,
            &Custom(template),
            &limits,
            &policy
        ),
        Err(expected)
    );
}

//! Template slot names are resolved by semantic analysis, including unused metadata.
use htlk_analyzer::{
    ExpressionTypeEnvironment, ExpressionTypeError, check_expression, check_prompt_template,
};
use htlk_executable::{cbor::Limits, *};

#[test]
fn unknown_slots_and_unused_parameters_fail_after_model_construction() {
    let limits = Limits::default();
    let parameter = Port::new(ValueType::primitive(PrimitiveType::String), true);
    for template in [
        PromptTemplate::new(
            vec![],
            vec![TemplatePart::Slot("missing".parse().unwrap())],
            &limits,
        )
        .unwrap(),
        PromptTemplate::new(
            vec![("unused".parse().unwrap(), parameter.clone())],
            vec![],
            &limits,
        )
        .unwrap(),
        PromptTemplate::new(
            vec![("declared".parse().unwrap(), parameter.clone())],
            vec![TemplatePart::Slot("other".parse().unwrap())],
            &limits,
        )
        .unwrap(),
    ] {
        let decoded = PromptTemplate::decode(&template.encode(&limits).unwrap(), &limits).unwrap();
        assert_eq!(decoded, template);
        assert_eq!(
            check_prompt_template(&decoded, &limits),
            Err(ExpressionError::TemplateParameterMismatch)
        );
        let mut env = ExpressionTypeEnvironment::default();
        env.templates
            .insert(decoded.digest(&limits).unwrap(), decoded);
        assert_eq!(
            check_expression(
                &Expression::literal(ScalarLiteral::Boolean(true)),
                ExpressionContext::Eval,
                &env,
                None,
                &limits
            ),
            Err(ExpressionTypeError::Expression(
                ExpressionError::TemplateParameterMismatch
            ))
        );
    }
    let name: Identifier = "name".parse().unwrap();
    let repeated = PromptTemplate::new(
        vec![(name.clone(), parameter)],
        vec![TemplatePart::Slot(name.clone()), TemplatePart::Slot(name)],
        &limits,
    )
    .unwrap();
    check_prompt_template(&repeated, &limits).unwrap();
}

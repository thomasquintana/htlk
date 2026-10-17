//! Specification reducer scenarios, independent state oracle and completion order.
use htlk_executable::{BuiltinType, Port, PortTable, ValueType, cbor::Limits, digest::Digest};
use htlk_runtime::{
    binding::*,
    failure::{FailureCode, RuntimeFailure},
    identity::ArtifactId,
    records::{SecurityLabels, SourceState},
};
use std::{cell::Cell, collections::BTreeMap};

fn id() -> ArtifactId {
    Digest::from_bytes([1; 32]).to_string().parse().unwrap()
}
fn labels(values: &[&str]) -> SecurityLabels {
    SecurityLabels::new(
        values.iter().map(|s| (*s).into()).collect(),
        &Limits::default(),
    )
    .unwrap()
}
fn guard(state: GuardState) -> GuardEvaluation {
    GuardEvaluation {
        state,
        influence: SecurityLabels::default(),
        causes: vec![],
    }
}
fn source(state: SourceState) -> LabeledSource {
    LabeledSource {
        state,
        influence: SecurityLabels::default(),
        cause: None,
    }
}
fn binding(state: GuardState, value: SourceState) -> BindingDecision {
    reduce_binding(
        &["edge".parse().unwrap()],
        |_| guard(state.clone()),
        |_| source(value.clone()),
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn a_late_second_true_guard_never_selects_or_dispatches_an_early_source() {
    let edges = vec!["late".parse().unwrap(), "early".parse().unwrap()];
    let calls = Cell::new(0);
    let mut order = Vec::new();
    let pending = reduce_binding(
        &edges,
        |edge| {
            order.push(edge.to_string());
            guard(if edge.as_str() == "early" {
                GuardState::Value(true)
            } else {
                GuardState::Pending
            })
        },
        |_| {
            calls.set(calls.get() + 1);
            source(SourceState::Value(id()))
        },
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(order, ["early", "late"]);
    assert_eq!(calls.get(), 0);
    assert!(matches!(
        pending,
        BindingDecision::Pending {
            selected_edge: None,
            ..
        }
    ));
    let conflict = reduce_binding(
        &edges,
        |_| guard(GuardState::Value(true)),
        |_| {
            calls.set(calls.get() + 1);
            source(SourceState::Value(id()))
        },
        &Limits::default(),
    )
    .unwrap();
    let BindingDecision::Failed { failures, .. } = conflict else {
        panic!()
    };
    assert_eq!(failures.len(), 2);
    assert!(
        failures
            .iter()
            .all(|f| f.error.code() == FailureCode::BindingConflict)
    );
    assert_eq!(calls.get(), 0);
}

#[test]
fn pending_guards_defer_errors_and_false_guards_never_wait_for_sources() {
    let edges = vec!["z".parse().unwrap(), "a".parse().unwrap()];
    let decision = reduce_binding(
        &edges,
        |edge| {
            guard(if edge.as_str() == "a" {
                GuardState::Error(RuntimeFailure::new(FailureCode::ExpressionAbsent))
            } else {
                GuardState::Pending
            })
        },
        |_| panic!("pending guards must not resolve sources"),
        &Limits::default(),
    )
    .unwrap();
    assert!(decision.is_pending());
    let BindingDecision::Pending { guard_failures, .. } = decision else {
        panic!()
    };
    assert_eq!(guard_failures[0].edge.as_str(), "a");
    assert_eq!(
        guard_failures[0].error.code(),
        FailureCode::ExpressionAbsent
    );
    let decision = reduce_binding(
        &edges,
        |_| guard(GuardState::Value(false)),
        |_| panic!("false edges must ignore even unavailable sources"),
        &Limits::default(),
    )
    .unwrap();
    assert!(matches!(
        decision,
        BindingDecision::Absent {
            selected_edge: None,
            ..
        }
    ));
    let decision = reduce_binding(
        &edges,
        |edge| {
            guard(GuardState::Error(RuntimeFailure::new(
                if edge.as_str() == "a" {
                    FailureCode::ExpressionAbsent
                } else {
                    FailureCode::Expression
                },
            )))
        },
        |_| panic!("guard errors precede source selection"),
        &Limits::default(),
    )
    .unwrap();
    let BindingDecision::Failed { failures, .. } = decision else {
        panic!()
    };
    assert_eq!(failures[0].edge.as_str(), "a");
    assert_eq!(failures[0].error.code(), FailureCode::ExpressionAbsent);
    assert_eq!(failures.len(), 2);
}

#[test]
fn sole_selected_source_keeps_pending_null_value_absence_and_unavailable_distinct() {
    assert!(binding(GuardState::Value(true), SourceState::Pending).is_pending());
    assert!(
        matches!(binding(GuardState::Value(true),SourceState::Value(id())),BindingDecision::Value{artifact_id,..} if artifact_id==id())
    );
    assert!(matches!(
        binding(GuardState::Value(true), SourceState::Absent),
        BindingDecision::Absent {
            selected_edge: Some(_),
            ..
        }
    ));
    for code in [FailureCode::Cancelled, FailureCode::McpTransport] {
        let decision = binding(
            GuardState::Value(true),
            SourceState::Unavailable(RuntimeFailure::new(code)),
        );
        let BindingDecision::Failed { failures, .. } = decision else {
            panic!()
        };
        assert_eq!(failures[0].error.code(), FailureCode::Dependency);
        assert_eq!(failures[0].source_error.as_ref().unwrap().code(), code);
    }
}

#[test]
fn absence_and_failure_keep_guard_and_selected_control_influences() {
    let edges = vec!["a".parse().unwrap(), "b".parse().unwrap()];
    let absent = reduce_binding(
        &edges,
        |edge| GuardEvaluation {
            state: GuardState::Value(false),
            influence: labels(&[if edge.as_str() == "a" {
                "secret-a"
            } else {
                "secret-b"
            }]),
            causes: vec![],
        },
        |_| panic!(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(absent.influence().as_slice(), &["secret-a", "secret-b"]);
    let failed = reduce_binding(
        &edges,
        |edge| GuardEvaluation {
            state: GuardState::Value(edge.as_str() == "a"),
            influence: labels(&["guard"]),
            causes: vec![],
        },
        |_| LabeledSource {
            state: SourceState::Unavailable(RuntimeFailure::new(FailureCode::Cancelled)),
            influence: labels(&["source"]),
            cause: None,
        },
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(failed.influence().as_slice(), &["guard", "source"]);
}

#[test]
fn generated_candidate_states_match_an_independent_settled_state_oracle() {
    // 0=pending, 1=false, 2=true, 3=error. Input order differs from canonical IDs.
    let edges = vec![
        "c".parse().unwrap(),
        "a".parse().unwrap(),
        "b".parse().unwrap(),
    ];
    for encoded in 0u32..64 {
        let states = [encoded & 3, (encoded >> 2) & 3, (encoded >> 4) & 3];
        let mut source_calls = 0;
        let result = reduce_binding(
            &edges,
            |edge| {
                let n = match edge.as_str() {
                    "a" => 0,
                    "b" => 1,
                    _ => 2,
                };
                guard(match states[n] {
                    0 => GuardState::Pending,
                    1 => GuardState::Value(false),
                    2 => GuardState::Value(true),
                    _ => GuardState::Error(RuntimeFailure::new(FailureCode::Expression)),
                })
            },
            |_| {
                source_calls += 1;
                source(SourceState::Value(id()))
            },
            &Limits::default(),
        )
        .unwrap();
        if states.contains(&0) {
            assert!(result.is_pending());
            assert_eq!(source_calls, 0);
            continue;
        }
        let errors = states
            .iter()
            .enumerate()
            .filter(|(_, s)| **s == 3)
            .map(|(n, _)| ["a", "b", "c"][n])
            .collect::<Vec<_>>();
        if !errors.is_empty() {
            let BindingDecision::Failed { failures, .. } = result else {
                panic!()
            };
            assert_eq!(
                failures.iter().map(|f| f.edge.as_str()).collect::<Vec<_>>(),
                errors
            );
            assert_eq!(source_calls, 0);
            continue;
        }
        let trues = states.iter().filter(|s| **s == 2).count();
        match trues {
            0 => {
                assert!(matches!(result, BindingDecision::Absent { .. }));
                assert_eq!(source_calls, 0)
            }
            1 => {
                assert!(matches!(result, BindingDecision::Value { .. }));
                assert_eq!(source_calls, 1)
            }
            _ => {
                let BindingDecision::Failed { failures, .. } = result else {
                    panic!()
                };
                assert_eq!(failures.len(), trues);
                assert_eq!(source_calls, 0)
            }
        }
    }
}
fn ports() -> PortTable {
    PortTable::new(
        vec![
            (
                "a".parse().unwrap(),
                Port::new(ValueType::builtin(BuiltinType::String), true),
            ),
            (
                "b".parse().unwrap(),
                Port::new(ValueType::builtin(BuiltinType::String), false),
            ),
        ],
        &Limits::default(),
    )
    .unwrap()
}

#[test]
fn guard_first_admission_ignores_even_invalid_input_inventories_when_inactive() {
    let invalid = BTreeMap::from([(
        "unknown".parse().unwrap(),
        BindingDecision::Failed {
            failures: vec![],
            influence: SecurityLabels::default(),
        },
    )]);
    assert!(matches!(
        reduce_admission(
            guard(GuardState::Value(false)),
            &ports(),
            &invalid,
            &Limits::default()
        )
        .unwrap(),
        AdmissionDecision::GuardSkipped(_)
    ));
    assert!(matches!(
        reduce_admission(
            guard(GuardState::Pending),
            &ports(),
            &invalid,
            &Limits::default()
        )
        .unwrap(),
        AdmissionDecision::Pending
    ));
    assert!(matches!(
        reduce_admission(
            guard(GuardState::Error(RuntimeFailure::new(
                FailureCode::Expression
            ))),
            &ports(),
            &invalid,
            &Limits::default()
        )
        .unwrap(),
        AdmissionDecision::GuardFailed(_)
    ));
}

#[test]
fn all_input_settlement_precedes_errors_which_precede_required_absence() {
    let failed = binding(
        GuardState::Value(true),
        SourceState::Unavailable(RuntimeFailure::new(FailureCode::McpTimeout)),
    );
    let mut inputs = BTreeMap::from([
        (
            "a".parse().unwrap(),
            binding(GuardState::Value(true), SourceState::Absent),
        ),
        (
            "b".parse().unwrap(),
            binding(GuardState::Pending, SourceState::Absent),
        ),
    ]);
    assert!(matches!(
        reduce_admission(
            guard(GuardState::Value(true)),
            &ports(),
            &inputs,
            &Limits::default()
        )
        .unwrap(),
        AdmissionDecision::Pending
    ));
    inputs.insert("b".parse().unwrap(), failed.clone());
    let AdmissionDecision::InputsFailed { failures, .. } = reduce_admission(
        guard(GuardState::Value(true)),
        &ports(),
        &inputs,
        &Limits::default(),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(failures[0].port.as_str(), "b");
    inputs.insert("a".parse().unwrap(), failed);
    let AdmissionDecision::InputsFailed { failures, .. } = reduce_admission(
        guard(GuardState::Value(true)),
        &ports(),
        &inputs,
        &Limits::default(),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        failures.iter().map(|f| f.port.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    inputs.insert(
        "a".parse().unwrap(),
        binding(GuardState::Value(true), SourceState::Absent),
    );
    inputs.insert(
        "b".parse().unwrap(),
        binding(GuardState::Value(false), SourceState::Absent),
    );
    assert!(
        matches!(reduce_admission(guard(GuardState::Value(true)),&ports(),&inputs,&Limits::default()).unwrap(),AdmissionDecision::RequiredAbsent{ports,..} if ports[0].as_str()=="a")
    );
    inputs.insert(
        "a".parse().unwrap(),
        binding(GuardState::Value(true), SourceState::Value(id())),
    );
    assert!(matches!(
        reduce_admission(
            guard(GuardState::Value(true)),
            &ports(),
            &inputs,
            &Limits::default()
        )
        .unwrap(),
        AdmissionDecision::InputsReady(_)
    ));
}

#[test]
fn malformed_plans_and_metadata_limits_do_not_call_sources_or_become_false() {
    let duplicate = vec!["a".parse().unwrap(), "a".parse().unwrap()];
    assert!(matches!(
        reduce_binding(&duplicate, |_| panic!(), |_| panic!(), &Limits::default()),
        Err(ReducerError::InvalidPlan)
    ));
    assert!(
        reduce_binding(
            &["a".parse().unwrap()],
            |_| guard(GuardState::Value(false)),
            |_| panic!(),
            &Limits {
                max_document_bytes: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(
        reduce_admission(
            guard(GuardState::Value(true)),
            &ports(),
            &BTreeMap::new(),
            &Limits::default()
        )
        .is_err()
    );
}

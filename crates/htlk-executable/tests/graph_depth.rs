//! Whole-record embedding depth, including nested expression cleanup.

use htlk_cbor::Limits;
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{
    BuiltinType as P, Edge, EdgeDestination as D, EdgeSource as S, Expression as E,
    ExpressionContext as EC, GraphRecordError as Error, Node, NodeFields, Operation, Port,
    PortTable, ScalarLiteral, Scope, ScopeContext as C, ScopeFields, ValueType,
};

fn fields(depth: usize, limits: &Limits) -> ScopeFields {
    let mut bytes = b"\x82\x63not".repeat(depth);
    bytes.extend_from_slice(b"\x82\x67literal\xf5");
    let expr = E::decode(&bytes, EC::Guard { loop_body: false }, limits).unwrap();
    let ports = |name: &str| {
        PortTable::new(
            vec![(
                name.parse().unwrap(),
                Port::new(ValueType::builtin(P::String), true),
            )],
            limits,
        )
        .unwrap()
    };
    let mut f = NodeFields::new(
        "worker".parse().unwrap(),
        Operation::Eval(E::literal(ScalarLiteral::String("x".into()))),
    );
    f.outputs = ports("value");
    f.guard = expr;
    let node = Node::new(f, C::Ordinary, limits).unwrap();
    ScopeFields {
        nodes: vec![node],
        outputs: ports("result"),
        edges: vec![Edge::new(
            "export".parse().unwrap(),
            S::Output {
                node: "worker".parse().unwrap(),
                port: "value".parse().unwrap(),
            },
            D::Output("result".parse().unwrap()),
        )],
        ..ScopeFields::default()
    }
}
fn exercise() {
    let limits = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    // Scope map -> nodes array -> node map -> guard expression: offset three.
    let scope = Scope::new(fields(124, &limits), C::Ordinary, &limits).unwrap();
    let bytes = scope.encode(C::Ordinary, &limits).unwrap();
    assert_eq!(Scope::decode(&bytes, C::Ordinary, &limits).unwrap(), scope);
    assert_eq!(scope.clone(), scope);
    assert!(matches!(
        Scope::new(fields(125, &limits), C::Ordinary, &limits),
        Err(Error::LimitExceeded { .. })
    ));
    let mut trailing = bytes;
    trailing.push(0);
    assert!(matches!(
        Scope::decode(&trailing, C::Ordinary, &limits),
        Err(Error::Codec(_))
    ));
}

#[test]
fn graph_records_on_controlled_stacks() {
    const CHILD: &str = "HTLK_GRAPH_DEPTH_STACK";
    if let Ok(size) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(size.parse().unwrap())
            .spawn(exercise)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for size in [512 * 1024, 2 * 1024 * 1024] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "graph_records_on_controlled_stacks",
                "--nocapture",
            ])
            .env(CHILD, size.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stack {size}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

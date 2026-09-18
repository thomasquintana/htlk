//! Subprocess-isolated expression depth and cleanup regressions.

use htlk_cbor::{Limits, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{
    Expression as E, ExpressionContext as C, ExpressionError as Error, ExpressionKind as K,
};

fn not_bytes(depth: usize) -> Vec<u8> {
    let mut bytes = b"\x82\x63not".repeat(depth);
    bytes.extend_from_slice(b"\x82\x67literal\xf5");
    bytes
}
fn not_value(depth: usize) -> Value {
    let mut v = Value::Array(vec![Value::Text("literal".into()), Value::Bool(true)]);
    for _ in 0..depth {
        v = Value::Array(vec![Value::Text("not".into()), v]);
    }
    v
}
fn exercise() {
    let limits = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    for n in [0, 63, 64, 126, 127] {
        eprintln!("expression depth {n}: decode");
        let bytes = not_bytes(n);
        let e = E::decode(&bytes, C::Eval, &limits).unwrap();
        eprintln!("expression depth {n}: clone/debug");
        assert_eq!(e.clone(), e);
        assert!(!format!("{e:?}").is_empty());
        eprintln!("expression depth {n}: encode");
        assert_eq!(e.encode(C::Eval, &limits).unwrap(), bytes);
        eprintln!("expression depth {n}: normalize");
        assert_eq!(E::new(e.kind().clone(), C::Eval, &limits).unwrap(), e);
    }
    eprintln!("over-depth and cleanup");
    let leaf = Value::Array(vec![Value::Text("literal".into()), Value::Bool(true)]);
    let mut binary = leaf.clone();
    for _ in 0..127 {
        binary = Value::Array(vec![Value::Text("and".into()), leaf.clone(), binary]);
    }
    let mut call = leaf;
    for _ in 0..63 {
        call = Value::Array(vec![
            Value::Text("call".into()),
            Value::Array(vec![
                Value::Text("core".into()),
                Value::Text("present".into()),
            ]),
            Value::Array(vec![call]),
        ]);
    }
    for value in [binary, call] {
        eprintln!("deep binary/call: decode and encode");
        let bytes = htlk_cbor::encode(&value, &limits).unwrap();
        let expr = E::decode(&bytes, C::Eval, &limits).unwrap();
        eprintln!("deep binary/call: encode");
        assert_eq!(expr.encode(C::Eval, &limits).unwrap(), bytes);
        eprintln!("deep binary/call: normalize");
        assert_eq!(E::new(expr.kind().clone(), C::Eval, &limits).unwrap(), expr);
    }
    assert!(matches!(
        E::decode(&not_bytes(128), C::Eval, &limits),
        Err(Error::Codec(_))
    ));
    let deepest = E::decode(&not_bytes(127), C::Eval, &limits).unwrap();
    eprintln!("over-depth constructor");
    assert!(matches!(
        E::new(K::Not(Box::new(deepest)), C::Eval, &limits),
        Err(Error::LimitExceeded { .. })
    ));
    // Each active binary parser retains a completed deep left subtree before
    // visiting the right subtree. Invalid final constructor exercises cleanup.
    eprintln!("partial-tree cleanup fixture");
    let mut value = Value::Array(vec![Value::Text("unknown".into())]);
    for index in (0..64).rev() {
        value = Value::Array(vec![
            Value::Text("and".into()),
            not_value(126 - index),
            value,
        ]);
    }
    let bytes = htlk_cbor::encode(&value, &limits).unwrap();
    eprintln!("partial-tree decode");
    assert_eq!(
        E::decode(&bytes, C::Eval, &limits).unwrap_err(),
        Error::UnknownConstructor
    );
    eprintln!("trailing-data cleanup");
    let mut trailing = not_bytes(127);
    trailing.push(0);
    assert!(matches!(
        E::decode(&trailing, C::Eval, &limits),
        Err(Error::Codec(_))
    ));
}

#[test]
fn expression_operations_on_controlled_stacks() {
    const CHILD: &str = "HTLK_EXPRESSION_DEPTH_STACK";
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
                "expression_operations_on_controlled_stacks",
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

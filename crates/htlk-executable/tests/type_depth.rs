//! Isolated depth and cleanup checks for the recursive canonical type model.

use htlk_cbor::{Limits, Map, Value};
use htlk_executable::{Port, TypeContext as C, TypeError, ValueType as T, ValueTypeKind as K};

fn list_bytes(depth: usize) -> Vec<u8> {
    let mut bytes = b"\x82\x64list".repeat(depth);
    bytes.extend_from_slice(b"\x66string");
    bytes
}

fn list_value(depth: usize) -> Value {
    let mut value = Value::Text("string".into());
    for _ in 0..depth {
        value = Value::Array(vec![Value::Text("list".into()), value]);
    }
    value
}

fn port(value: Value) -> Value {
    Value::Map(
        Map::try_from_entries([
            ("type".into(), value),
            ("required".into(), Value::Bool(false)),
        ])
        .unwrap(),
    )
}

fn exercise() {
    let limits = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    for depth in [0, 64, 65, 127, 128] {
        eprintln!("depth {depth}: decode");
        let bytes = list_bytes(depth);
        let ty = T::decode(&bytes, C::Value, &limits).unwrap();
        eprintln!("depth {depth}: clone/equality/debug");
        let clone = ty.clone();
        assert_eq!(ty, clone);
        assert!(!format!("{ty:?}").is_empty());
        eprintln!("depth {depth}: encode");
        assert_eq!(ty.encode(C::Value, &limits).unwrap(), bytes);
        eprintln!("depth {depth}: construct normalized type");
        assert_eq!(T::new(ty.kind().clone(), C::Value, &limits).unwrap(), ty);
        if depth > 64 {
            assert!(T::decode(&bytes, C::Value, &Limits::default()).is_err());
        }
        drop(clone);
        drop(ty);
    }
    assert!(matches!(
        T::decode(&list_bytes(129), C::Value, &limits),
        Err(TypeError::Codec(_))
    ));
    let ty = T::decode(&list_bytes(128), C::Value, &limits).unwrap();
    assert!(matches!(
        T::new(K::List(Box::new(ty)), C::Value, &limits),
        Err(TypeError::LimitExceeded { .. })
    ));
    let p = Port::new(
        T::decode(&list_bytes(127), C::Value, &limits).unwrap(),
        true,
    );
    let bytes = p.encode(C::Value, &limits).unwrap();
    assert_eq!(Port::decode(&bytes, C::Value, &limits).unwrap(), p);

    eprintln!("deep function signatures and unions");
    let mut function = Value::Text("string".into());
    for _ in 0..64 {
        function = Value::Array(vec![
            Value::Text("function".into()),
            Value::Array(vec![]),
            port(function),
        ]);
    }
    let mut union = Value::Text("string".into());
    for _ in 0..42 {
        union = Value::Array(vec![
            Value::Text("union".into()),
            Value::Array(vec![
                Value::Text("null".into()),
                Value::Array(vec![Value::Text("list".into()), union]),
            ]),
        ]);
    }
    for (value, context) in [(function, C::Signature), (union, C::Value)] {
        let bytes = htlk_cbor::encode(&value, &limits).unwrap();
        let ty = T::decode(&bytes, context, &limits).unwrap();
        assert_eq!(ty.encode(context, &limits).unwrap(), bytes);
        assert_eq!(T::new(ty.kind().clone(), context, &limits).unwrap(), ty);
    }

    // Each record keeps an already parsed deep 'a' field while entering its 'b'
    // field. The final invalid type must drop all retained subtrees on error.
    let mut value = Value::Text("unknown_type".into());
    for index in (0..42).rev() {
        let fields = Map::try_from_entries([
            ("a".into(), port(list_value(125 - 3 * index))),
            ("b".into(), port(value)),
        ])
        .unwrap();
        value = Value::Array(vec![Value::Text("record".into()), Value::Map(fields)]);
    }
    let bytes = htlk_cbor::encode(&value, &limits).unwrap();
    assert_eq!(
        T::decode(&bytes, C::Value, &limits).unwrap_err(),
        TypeError::UnknownPrimitive
    );
    drop(value);
}

#[test]
fn type_operations_on_controlled_stacks() {
    const CHILD: &str = "HTLK_TYPE_DEPTH_STACK";
    if let Ok(stack) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(stack.parse().unwrap())
            .spawn(exercise)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for stack in [512 * 1024, 2 * 1024 * 1024] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "type_operations_on_controlled_stacks",
                "--nocapture",
            ])
            .env(CHILD, stack.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stack {stack}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

//! Isolated stack-budget regression checks for recursive value operations.

use htlk_executable::cbor::{ErrorKind, LimitKind, Limits, Map, Value, decode, encode};

fn nested(depth: usize, maps: bool) -> Value {
    let mut value = Value::Null;
    for _ in 0..depth {
        value = if maps {
            Value::Map(Map::try_from_entries([("x".into(), value)]).unwrap())
        } else {
            Value::Array(vec![value])
        };
    }
    value
}

#[test]
fn recursive_operations_on_controlled_stacks() {
    // Isolate a stack-overflow abort from the parent test process. Invoke the
    // same test executable and select this exact test to avoid recursive spawn.
    const CHILD: &str = "HTLK_CBOR_DEPTH_TEST_STACK";
    if let Ok(stack) = std::env::var(CHILD) {
        let stack: usize = stack.parse().unwrap();
        std::thread::Builder::new()
            .stack_size(stack)
            .spawn(|| {
                for maps in [false, true] {
                    for depth in [0, 64, 65, 128] {
                        let value = nested(depth, maps);
                        let copy = value.clone();
                        assert_eq!(value, copy);
                        assert!(!format!("{value:?}").is_empty());
                        let limits = Limits {
                            max_depth: 128,
                            ..Limits::default()
                        };
                        let bytes = encode(&value, &limits).unwrap();
                        let decoded = decode(&bytes, &limits).unwrap();
                        assert_eq!(decoded, value);
                        drop(decoded);
                        if depth > 64 {
                            assert!(matches!(
                                decode(&bytes, &Limits::default()).unwrap_err().kind(),
                                ErrorKind::LimitExceeded {
                                    limit: LimitKind::Depth,
                                    maximum: 64
                                }
                            ));
                            assert_eq!(
                                encode(&value, &Limits::default()).unwrap_err().kind(),
                                &ErrorKind::LimitExceeded {
                                    limit: LimitKind::Depth,
                                    maximum: 64
                                }
                            );
                        }
                        drop(copy);
                        drop(value);
                    }
                    let too_deep = nested(129, maps);
                    assert_eq!(
                        encode(
                            &too_deep,
                            &Limits {
                                max_depth: 128,
                                ..Limits::default()
                            }
                        )
                        .unwrap_err()
                        .kind(),
                        &ErrorKind::LimitExceeded {
                            limit: LimitKind::Depth,
                            maximum: 128
                        }
                    );
                    drop(too_deep);
                    decoder_cleanup(maps);
                }
            })
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    // 512 KiB exercises a 4x smaller stack than the 2 MiB regression baseline.
    for stack in [512 * 1024, 2 * 1024 * 1024] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "recursive_operations_on_controlled_stacks",
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

fn decoder_cleanup(maps: bool) {
    let limits = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    // Reject depth 129 without constructing an over-depth owned value.
    let prefix: &[u8] = if maps { &[0xa1, 0x61, b'x'] } else { &[0x81] };
    let mut bytes = prefix.repeat(129);
    bytes.push(0xf6);
    assert!(matches!(
        decode(&bytes, &limits).unwrap_err().kind(),
        ErrorKind::LimitExceeded {
            limit: LimitKind::Depth,
            maximum: 128
        }
    ));

    // Each active decoder frame retains a completed subtree while descending
    // into the next sibling. Failure at the bottom must unwind and drop all
    // those subtrees while decoder call frames still occupy the stack.
    let mut bytes = Vec::new();
    for depth in 0..128 {
        if maps {
            bytes.extend_from_slice(&[0xa2, 0x61, b'a']);
        } else {
            bytes.push(0x82);
        }
        bytes.extend_from_slice(&encode(&nested(127 - depth, maps), &limits).unwrap());
        if maps {
            bytes.extend_from_slice(&[0x61, b'b']);
        }
    }
    bytes.push(0xff);
    let error = decode(&bytes, &limits).unwrap_err();
    assert_eq!(error.kind(), &ErrorKind::UnsupportedType);
    assert_eq!(error.offset(), Some(bytes.len() - 1));

    // A trailing-data error must drop a completely decoded depth-128 value.
    let mut bytes = encode(&nested(128, maps), &limits).unwrap();
    bytes.push(0);
    assert_eq!(
        decode(&bytes, &limits).unwrap_err().kind(),
        &ErrorKind::TrailingData
    );
}

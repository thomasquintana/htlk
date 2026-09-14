//! Embedded library-signature depth and cleanup regression checks.

use htlk_cbor::Limits;
use htlk_executable::digest::Digest;
use htlk_executable::{FunctionSignature, Library, MetadataError, Port, TypeContext, ValueType};

fn signature(depth: usize, l: &Limits) -> FunctionSignature {
    let mut wire = b"\x82\x64list".repeat(depth);
    wire.extend_from_slice(b"\x66string");
    let ty = ValueType::decode(&wire, TypeContext::Signature, l).unwrap();
    FunctionSignature::new(
        vec![],
        vec![Port::new(ty, true)],
        Port::new(
            ValueType::primitive(htlk_executable::PrimitiveType::Boolean),
            true,
        ),
        l,
    )
    .unwrap()
}
fn exercise() {
    let l = Limits {
        max_depth: 128,
        ..Limits::default()
    };
    // library -> functions -> signature -> parameters -> port -> type adds five.
    let library = Library::new(
        "test".into(),
        "0.1".into(),
        Digest::from_bytes([0; 32]),
        vec![("f".parse().unwrap(), signature(123, &l))],
        &l,
    )
    .unwrap();
    let bytes = library.encode(&l).unwrap();
    assert_eq!(Library::decode(&bytes, &l).unwrap(), library);
    assert!(matches!(
        Library::new(
            "test".into(),
            "0.1".into(),
            Digest::from_bytes([0; 32]),
            vec![("f".parse().unwrap(), signature(124, &l))],
            &l
        ),
        Err(MetadataError::LimitExceeded { .. })
    ));
    let mut trailing = bytes;
    trailing.push(0);
    assert!(matches!(
        Library::decode(&trailing, &l),
        Err(MetadataError::Codec(_))
    ));
}
#[test]
fn metadata_on_controlled_stacks() {
    const CHILD: &str = "HTLK_METADATA_DEPTH_STACK";
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
            .args(["--exact", "metadata_on_controlled_stacks", "--nocapture"])
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

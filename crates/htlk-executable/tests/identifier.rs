//! Canonical identifier spelling, error positions, and consumer-facing traits.

use std::collections::{BTreeMap, HashMap};

use htlk_cbor::{Limits, Map, Value};
use htlk_executable::cbor as htlk_cbor;
use htlk_executable::{Identifier, ParseIdentifierError};

#[test]
fn valid_names_preserve_exact_spelling_across_construction_paths() {
    for text in [
        "a",
        "z",
        "question",
        "draft_v2",
        "a0_1_b2",
        "a_0",
        "review_result",
    ] {
        let parsed = text.parse::<Identifier>().unwrap();
        assert_eq!(parsed.as_str(), text);
        assert_eq!(parsed.as_ref(), text);
        assert_eq!(parsed.to_string(), text);
        assert_eq!(Identifier::new(text.to_owned()).unwrap(), parsed);
        assert_eq!(Identifier::try_from(text).unwrap(), parsed);
        assert_eq!(Identifier::try_from(text.to_owned()).unwrap(), parsed);
        assert_eq!(parsed.into_string(), text);
    }
    let long = "a".repeat(4096);
    assert_eq!(long.parse::<Identifier>().unwrap().as_str(), long);
}

#[test]
fn invalid_names_report_precise_spelling_errors() {
    use ParseIdentifierError::*;
    for (text, expected) in [
        ("", Empty),
        ("A", InvalidStart),
        ("1name", InvalidStart),
        ("_name", InvalidStart),
        (" name", InvalidStart),
        ("é", InvalidStart),
        ("\u{feff}name", InvalidStart),
        ("a_", InvalidSeparator { offset: 1 }),
        ("a__b", InvalidSeparator { offset: 2 }),
        ("a___", InvalidSeparator { offset: 2 }),
        ("name_", InvalidSeparator { offset: 4 }),
        ("aB", InvalidCharacter { offset: 1 }),
        ("a_bC", InvalidCharacter { offset: 3 }),
        ("a-b", InvalidCharacter { offset: 1 }),
        ("a.b", InvalidCharacter { offset: 1 }),
        ("a/b", InvalidCharacter { offset: 1 }),
        ("a\\b", InvalidCharacter { offset: 1 }),
        ("name ", InvalidCharacter { offset: 4 }),
        ("a\n", InvalidCharacter { offset: 1 }),
        ("a\t", InvalidCharacter { offset: 1 }),
        ("a\0", InvalidCharacter { offset: 1 }),
        ("a_é", InvalidCharacter { offset: 2 }),
        ("ab\u{301}", InvalidCharacter { offset: 2 }),
        ("a_１", InvalidCharacter { offset: 2 }),
    ] {
        assert_eq!(text.parse::<Identifier>(), Err(expected), "{text:?}");
        assert_eq!(Identifier::new(text.to_owned()), Err(expected), "{text:?}");
    }
}

#[test]
fn keyword_and_reserved_root_rules_are_contextual() {
    // CDDL's lexical rule does not prohibit these. Source/node/declaration
    // validators will apply reserved-root restrictions in their own contexts.
    for text in [
        "graph",
        "task",
        "type",
        "wait",
        "true",
        "self",
        "inputs",
        "outputs",
        "carried",
        "next",
        "length",
        "present",
        "status",
        "error",
        "render",
        "mcp",
        "predicates",
    ] {
        assert_eq!(text.parse::<Identifier>().unwrap().as_str(), text);
    }
}

#[test]
fn borrowed_map_lookups_and_lexical_order_are_consistent() {
    let entries: Vec<_> = ["z", "aa", "a_1"]
        .into_iter()
        .map(|text| (text.parse::<Identifier>().unwrap(), text.len()))
        .collect();
    let hashed: HashMap<_, _> = entries.clone().into_iter().collect();
    let ordered: BTreeMap<_, _> = entries.into_iter().collect();
    assert_eq!(hashed.get("aa"), Some(&2));
    assert_eq!(ordered.get("a_1"), Some(&3));
    assert_eq!(hashed.get("missing"), None);
    assert_eq!(
        ordered.keys().map(Identifier::as_str).collect::<Vec<_>>(),
        ["a_1", "aa", "z"]
    );
    // CBOR map ordering differs from lexical identifier ordering: encoded key
    // bytes place shorter text keys first. No identifier-specific encoding tag.
    let value = Value::Map(
        Map::try_from_entries(
            ordered
                .keys()
                .cloned()
                .map(|key| (key.into_string(), Value::Null)),
        )
        .unwrap(),
    );
    let bytes = htlk_cbor::encode(&value, &Limits::default()).unwrap();
    assert_eq!(bytes, b"\xa3\x61z\xf6\x62aa\xf6\x63a_1\xf6");
}

#[test]
fn errors_do_not_retain_input_contents() {
    let error = "private-input-content".parse::<Identifier>().unwrap_err();
    assert_eq!(error, ParseIdentifierError::InvalidCharacter { offset: 7 });
    assert!(!error.to_string().contains("private-input-content"));
    assert!(!format!("{error:?}").contains("private-input-content"));
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn bounded_exhaustive_names_match_the_segment_grammar() {
    // Independent structural reading of the spec: a nonempty initial word with
    // a lowercase first letter, followed by zero or more nonempty words.
    fn accepts(text: &str) -> bool {
        let word = |part: &str| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        };
        let mut parts = text.split('_');
        let first = parts.next().unwrap();
        first.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            && word(first)
            && parts.all(word)
    }
    let alphabet = ['a', 'z', '0', '_', 'A', '-', '.', 'é'];
    for length in 0..=5 {
        for mut index in 0..alphabet.len().pow(length) {
            let mut text = String::new();
            for _ in 0..length {
                text.push(alphabet[index % alphabet.len()]);
                index /= alphabet.len();
            }
            assert_eq!(
                text.parse::<Identifier>().is_ok(),
                accepts(&text),
                "{text:?}"
            );
        }
    }
}

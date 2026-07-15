use humanize_plugin::flow::{FactRef, FlowPredicate, FlowRoute};
use serde_json::json;

#[test]
fn route_predicate_and_fanout_use_typed_structured_serde() {
    let route: FlowRoute = serde_json::from_value(json!({
        "predicate": {
            "op": "exists",
            "fact": { "kind": "artifact", "key": "ready" }
        },
        "for_each": { "key": "items" },
        "activate": "worker"
    }))
    .unwrap();
    let value = serde_json::to_value(route).unwrap();

    assert_eq!(value["predicate"]["op"], "exists");
    assert_eq!(value["predicate"]["fact"]["kind"], "artifact");
    assert_eq!(value["predicate"]["fact"]["key"], "ready");
    assert_eq!(value["for_each"]["key"], "items");
    assert!(
        serde_json::from_value::<FlowRoute>(json!({
            "predicate": "exists(artifact.ready)",
            "for_each": "artifact.items",
            "activate": "worker"
        }))
        .is_err()
    );
}

#[test]
fn typed_predicate_has_one_artifact_and_board_key_grammar() {
    let cases = [
        (
            json!({"op": "exists", "fact": {"kind": "artifact", "key": "result"}}),
            FlowPredicate::exists_artifact("result").unwrap(),
            "exists(artifact.result)",
        ),
        (
            json!({"op": "truthy", "fact": {"kind": "board", "key": "approved"}}),
            FlowPredicate::truthy_board("approved").unwrap(),
            "truthy(board.approved)",
        ),
    ];

    for (input, expected, display) in cases {
        let parsed = serde_json::from_value::<FlowPredicate>(input).unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(parsed.to_string(), display);
    }

    for key in [
        "",
        ".missing",
        "missing.",
        "bad-key",
        "bad..key",
        "caf\u{e9}",
    ] {
        for kind in ["artifact", "board"] {
            assert!(
                serde_json::from_value::<FlowPredicate>(json!({
                    "op": "exists",
                    "fact": {"kind": kind, "key": key}
                }))
                .is_err(),
                "{kind}.{key} should be rejected"
            );
        }
    }
    assert!(
        serde_json::from_value::<FlowPredicate>(json!({
            "op": "exists",
            "fact": {"kind": "event", "key": "started"}
        }))
        .is_err()
    );
}

#[test]
fn exists_and_truthy_share_one_value_semantics() {
    let exists = FlowPredicate::exists(FactRef::artifact("result").unwrap());
    for value in [Some("value"), Some(""), Some("false"), Some("0")] {
        assert!(exists.matches(value));
    }
    assert!(!exists.matches(None));

    let truthy = FlowPredicate::truthy(FactRef::board("approved").unwrap());
    for value in [None, Some(""), Some("  "), Some("false"), Some("0")] {
        assert!(!truthy.matches(value), "{value:?} should be false");
    }
    for value in [Some("true"), Some("1"), Some("approved")] {
        assert!(truthy.matches(value), "{value:?} should be true");
    }
}

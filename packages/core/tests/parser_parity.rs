use taladb::json::{filter_from_json, update_from_json};

#[test]
fn canonical_parser_acceptance_fixture() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/parser_parity.json")).unwrap();
    for case in cases["filters"].as_array().unwrap() {
        assert_eq!(
            filter_from_json(&case["input"]).is_ok(),
            case["valid"].as_bool().unwrap(),
            "{case}"
        );
    }
    for case in cases["updates"].as_array().unwrap() {
        assert_eq!(
            update_from_json(&case["input"]).is_ok(),
            case["valid"].as_bool().unwrap(),
            "{case}"
        );
    }
}

#[test]
fn canonical_update_rejects_excessive_depth_before_conversion() {
    let mut value = serde_json::json!(1);
    for _ in 0..taladb::json_depth::MAX_JSON_DEPTH {
        value = serde_json::json!({"child": value});
    }
    assert!(update_from_json(&serde_json::json!({"$set": {"field": value}})).is_err());
}

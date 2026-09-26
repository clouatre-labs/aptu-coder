//! Tests for `slim_output_schema` and output-schema surface telemetry data.

use crate::slim_output_schema;
use serde_json::Value;

fn assert_no_descriptions(value: &Value) {
    match value {
        Value::Object(map) => {
            assert!(
                !map.contains_key("description"),
                "found description in {map:?}"
            );
            for v in map.values() {
                assert_no_descriptions(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                assert_no_descriptions(v);
            }
        }
        _ => {}
    }
}

#[test]
fn slim_output_schema_strips_all_description_keys_recursively() {
    for slim in [
        slim_output_schema::<crate::ShellOutputMetadata>(),
        slim_output_schema::<crate::EditReplaceOutputMetadata>(),
    ] {
        let value = Value::Object((*slim).clone());
        assert_no_descriptions(&value);
    }
}

#[test]
fn slim_output_schema_does_not_mutate_schema_for_type_cache() {
    // rmcp caches the generated Arc<JsonObject> per TypeId; slimming must clone
    // before stripping so the cached entry keeps its descriptions.
    let before = rmcp::handler::server::tool::schema_for_type::<crate::ShellOutputMetadata>();
    let _ = slim_output_schema::<crate::ShellOutputMetadata>();
    let after = rmcp::handler::server::tool::schema_for_type::<crate::ShellOutputMetadata>();
    assert_eq!(
        serde_json::to_string(&*before).unwrap(),
        serde_json::to_string(&*after).unwrap(),
        "schema_for_type cache entry must be unchanged after slim runs"
    );
    assert!(
        serde_json::to_string(&*after)
            .unwrap()
            .contains("description"),
        "unslimmed schema_for_type result keeps descriptions"
    );
}

#[test]
fn slim_output_schema_reduces_serialized_size() {
    for (slim, unslimmed) in [
        (
            slim_output_schema::<crate::ShellOutputMetadata>(),
            rmcp::handler::server::tool::schema_for_type::<crate::ShellOutputMetadata>(),
        ),
        (
            slim_output_schema::<crate::EditReplaceOutputMetadata>(),
            rmcp::handler::server::tool::schema_for_type::<crate::EditReplaceOutputMetadata>(),
        ),
    ] {
        let slim_len = serde_json::to_string(&*slim).unwrap().len();
        let full_len = serde_json::to_string(&*unslimmed).unwrap().len();
        assert!(
            slim_len < full_len,
            "slim schema ({slim_len} bytes) must be smaller than original ({full_len} bytes)"
        );
    }
}

#[test]
fn list_tools_output_schemas_are_slimmed() {
    let tools = crate::CodeAnalyzer::list_tools();
    assert!(!tools.is_empty());
    for tool in &tools {
        if let Some(schema) = &tool.output_schema {
            assert_no_descriptions(&Value::Object((**schema).clone()));
        }
    }
}

#[test]
fn metrics_export_omits_output_schema_chars_on_ordinary_events() {
    let event = crate::metrics::MetricEventBuilder::new("analyze_file", "ok", 10).build();
    let json = serde_json::to_string(&event).unwrap();
    assert!(!json.contains("output_schema_chars"));
}

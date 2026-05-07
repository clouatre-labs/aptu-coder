// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use aptu_coder::CodeAnalyzer;
use serde_json::Value;

#[test]
fn test_all_tools_have_correct_annotations() {
    let tools = CodeAnalyzer::list_tools();

    assert_eq!(tools.len(), 10, "expected 10 registered tools");

    let expected_names = [
        "analyze_directory",
        "analyze_file",
        "analyze_module",
        "analyze_symbol",
        "analyze_raw",
        "edit_overwrite",
        "edit_replace",
        "edit_rename",
        "edit_insert",
        "exec_command",
    ];

    for tool in &tools {
        let name = tool.name.as_ref();
        assert!(
            expected_names.contains(&name),
            "unexpected tool name: {}",
            name
        );

        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("tool {} is missing annotations", name));

        // edit_overwrite, edit_replace, edit_insert, and exec_command are destructive; edit_rename is idempotent; others are read-only
        if name == "edit_overwrite"
            || name == "edit_replace"
            || name == "edit_insert"
            || name == "exec_command"
        {
            assert_eq!(
                annotations.read_only_hint,
                Some(false),
                "tool {} must have read_only_hint=false",
                name
            );
            assert_eq!(
                annotations.destructive_hint,
                Some(true),
                "tool {} must have destructive_hint=true",
                name
            );
            assert_eq!(
                annotations.idempotent_hint,
                Some(false),
                "tool {} must have idempotent_hint=false",
                name
            );
        } else if name == "edit_rename" {
            assert_eq!(
                annotations.read_only_hint,
                Some(false),
                "tool {} must have read_only_hint=false",
                name
            );
            assert_eq!(
                annotations.destructive_hint,
                Some(false),
                "tool {} must have destructive_hint=false",
                name
            );
            assert_eq!(
                annotations.idempotent_hint,
                Some(true),
                "tool {} must have idempotent_hint=true",
                name
            );
        } else {
            assert_eq!(
                annotations.read_only_hint,
                Some(true),
                "tool {} must have read_only_hint=true",
                name
            );
            assert_eq!(
                annotations.destructive_hint,
                Some(false),
                "tool {} must have destructive_hint=false",
                name
            );
            assert_eq!(
                annotations.idempotent_hint,
                Some(true),
                "tool {} must have idempotent_hint=true",
                name
            );
        }

        // exec_command has open_world_hint=true; others have open_world_hint=false
        if name == "exec_command" {
            assert_eq!(
                annotations.open_world_hint,
                Some(true),
                "tool {} must have open_world_hint=true",
                name
            );
        } else {
            assert_eq!(
                annotations.open_world_hint,
                Some(false),
                "tool {} must have open_world_hint=false",
                name
            );
        }
    }
}

#[test]
fn test_all_tools_have_non_empty_descriptions() {
    let tools = CodeAnalyzer::list_tools();
    for tool in &tools {
        let name = tool.name.as_ref();
        let desc = tool.description.as_deref().unwrap_or("");
        assert!(
            !desc.is_empty(),
            "tool '{}' has an empty or missing description",
            name
        );
    }
}

#[test]
fn test_all_tool_parameters_have_descriptions() {
    let tools = CodeAnalyzer::list_tools();
    for tool in &tools {
        let tool_name = tool.name.as_ref();
        let schema = &tool.input_schema;
        let properties = match schema.get("properties").and_then(Value::as_object) {
            Some(p) => p,
            None => continue,
        };
        for (param_name, param_schema) in properties.iter() {
            let desc = param_schema
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                !desc.is_empty(),
                "tool '{}' parameter '{}' has an empty or missing description in inputSchema",
                tool_name,
                param_name
            );
        }
    }
}

#[test]
fn test_flatten_fields_have_descriptions() {
    let tools = CodeAnalyzer::list_tools();
    let flatten_fields = ["cursor", "page_size", "summary", "force", "verbose"];
    for tool in &tools {
        let tool_name = tool.name.as_ref();
        if tool_name == "analyze_module" {
            continue;
        }
        let properties = match tool
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
        {
            Some(p) => p,
            None => continue,
        };
        for field in &flatten_fields {
            if let Some(param_schema) = properties.get(*field) {
                let desc = param_schema
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                assert!(
                    !desc.is_empty(),
                    "tool '{}' flattened field '{}' is missing a description in inputSchema",
                    tool_name,
                    field
                );
            }
        }
    }
}

#[test]
fn test_complex_params_have_examples() {
    let tools = CodeAnalyzer::list_tools();
    let checks: &[(&str, &str)] = &[("analyze_file", "fields"), ("analyze_symbol", "match_mode")];
    for (tool_name, param_name) in checks {
        let tool = tools
            .iter()
            .find(|t| t.name.as_ref() == *tool_name)
            .unwrap_or_else(|| panic!("tool '{}' not found", tool_name));
        let properties = tool
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("tool '{}' has no properties", tool_name));
        let param = properties
            .get(*param_name)
            .unwrap_or_else(|| panic!("tool '{}' has no parameter '{}'", tool_name, param_name));
        let examples = param.get("examples").and_then(Value::as_array);
        assert!(
            examples.map_or(false, |arr| !arr.is_empty()),
            "tool '{}' parameter '{}' is missing a JSON Schema 'examples' array",
            tool_name,
            param_name
        );
    }
}

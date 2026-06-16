// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;
use tracing_subscriber::filter::LevelFilter;

/// Serializes tests that mutate process-global env vars to prevent parallel pollution.
/// Returns a `tokio::sync::MutexGuard` that can be held across `.await` points.
async fn env_var_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    let m = LOCK.get_or_init(|| TokioMutex::new(()));
    m.lock().await
}

fn make_test_analyzer() -> aptu_coder::CodeAnalyzer {
    let peer = Arc::new(TokioMutex::new(None));
    let log_level_filter = Arc::new(Mutex::new(LevelFilter::INFO));
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<aptu_coder::logging::LogEvent>();
    let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::unbounded_channel();
    aptu_coder::CodeAnalyzer::new(
        peer,
        log_level_filter,
        rx,
        aptu_coder::metrics::MetricsSender(metrics_tx),
    )
}

async fn call_tools_list_with_profile(profile: Option<&str>) -> serde_json::Value {
    let analyzer = make_test_analyzer();
    let (client, server) = tokio::io::duplex(65536);

    // Spawn the analyzer server on the server half
    let mut server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = rmcp::serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

    // Step 1: Send initialize request with optional profile in _meta
    let mut init_params = serde_json::json!({
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "test-client", "version": "0.1.0"}
    });

    if let Some(p) = profile {
        init_params["_meta"] = serde_json::json!({
            "io.clouatre-labs/profile": p
        });
    }

    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": init_params
    })
    .to_string()
        + "\n";
    client_tx.write_all(init.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 2: Read initialize response (discard)
    let _resp = reader.next_line().await.unwrap().unwrap();

    // Step 3: Send initialized notification (no id)
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    })
    .to_string()
        + "\n";
    client_tx.write_all(notif.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 4: Send tools/list request
    let list_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    })
    .to_string()
        + "\n";
    client_tx.write_all(list_req.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 5: Race response loop against server handle to surface server panics
    tokio::select! {
        result = async {
            loop {
                let line = reader.next_line().await.unwrap().unwrap();
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                if v.get("id") == Some(&serde_json::json!(2)) {
                    return v;
                }
            }
        } => {
            server_handle.abort();
            result
        }
        outcome = &mut server_handle => {
            match outcome {
                Ok(_) => panic!("server task exited unexpectedly before tools/list response"),
                Err(e) => panic!("server task panicked: {e}"),
            }
        }
    }
}

async fn call_tool_with_profile(profile: Option<&str>, tool_name: &str) -> serde_json::Value {
    let analyzer = make_test_analyzer();
    let (client, server) = tokio::io::duplex(65536);

    // Spawn the analyzer server on the server half
    let mut server_handle = tokio::spawn(async move {
        let (server_rx, server_tx) = tokio::io::split(server);
        if let Ok(service) = rmcp::serve_server(analyzer, (server_rx, server_tx)).await {
            let _ = service.waiting().await;
        }
    });

    let (client_rx, mut client_tx) = tokio::io::split(client);
    let mut reader = BufReader::new(client_rx).lines();

    // Step 1: Send initialize request with optional profile in _meta
    let mut init_params = serde_json::json!({
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "test-client", "version": "0.1.0"}
    });

    if let Some(p) = profile {
        init_params["_meta"] = serde_json::json!({
            "io.clouatre-labs/profile": p
        });
    }

    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": init_params
    })
    .to_string()
        + "\n";
    client_tx.write_all(init.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 2: Read initialize response (discard)
    let _resp = reader.next_line().await.unwrap().unwrap();

    // Step 3: Send initialized notification (no id)
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    })
    .to_string()
        + "\n";
    client_tx.write_all(notif.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 4: Send tools/call request
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": {}
        }
    })
    .to_string()
        + "\n";
    client_tx.write_all(call.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    // Step 5: Race response loop against server handle to surface server panics
    tokio::select! {
        result = async {
            loop {
                let line = reader.next_line().await.unwrap().unwrap();
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                if v.get("id") == Some(&serde_json::json!(2)) {
                    return v;
                }
            }
        } => {
            server_handle.abort();
            result
        }
        outcome = &mut server_handle => {
            match outcome {
                Ok(_) => panic!("server task exited unexpectedly before tool response"),
                Err(e) => panic!("server task panicked: {e}"),
            }
        }
    }
}

#[tokio::test]
async fn test_edit_profile_tool_count() {
    // Arrange: initialize with edit profile
    let resp = call_tools_list_with_profile(Some("edit")).await;

    // Act: extract tool count from response
    let tools = &resp["result"]["tools"];
    let tool_count = tools.as_array().map(|a| a.len()).unwrap_or(0);
    let tool_names: Vec<String> = tools
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
        .collect();

    // Assert: edit profile should have exactly 3 tools
    assert_eq!(
        tool_count, 3,
        "edit profile should enable exactly 3 tools, got: {:?}",
        tool_names
    );

    // Verify the correct tools are present
    assert!(
        tool_names.contains(&"edit_replace".to_string()),
        "edit profile should include edit_replace"
    );
    assert!(
        tool_names.contains(&"edit_overwrite".to_string()),
        "edit profile should include edit_overwrite"
    );
    assert!(
        tool_names.contains(&"exec_command".to_string()),
        "edit profile should include exec_command"
    );
}

#[tokio::test]
async fn test_analyze_profile_tool_count() {
    // Arrange: initialize with analyze profile
    let resp = call_tools_list_with_profile(Some("analyze")).await;

    // Act: extract tool count from response
    let tools = &resp["result"]["tools"];
    let tool_count = tools.as_array().map(|a| a.len()).unwrap_or(0);
    let tool_names: Vec<String> = tools
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
        .collect();

    // Assert: analyze profile should have exactly 5 tools
    assert_eq!(
        tool_count, 5,
        "analyze profile should enable exactly 5 tools, got: {:?}",
        tool_names
    );

    // Verify the correct tools are present
    assert!(
        tool_names.contains(&"analyze_directory".to_string()),
        "analyze profile should include analyze_directory"
    );
    assert!(
        tool_names.contains(&"analyze_file".to_string()),
        "analyze profile should include analyze_file"
    );
    assert!(
        tool_names.contains(&"analyze_module".to_string()),
        "analyze profile should include analyze_module"
    );
    assert!(
        tool_names.contains(&"analyze_symbol".to_string()),
        "analyze profile should include analyze_symbol"
    );
    assert!(
        tool_names.contains(&"exec_command".to_string()),
        "analyze profile should include exec_command"
    );
}

#[tokio::test]
async fn test_no_profile_tool_count() {
    // Arrange: hold guard across env var mutation and async call
    let _guard = env_var_lock().await;
    // Arrange: initialize with no profile metadata; env var must be absent.
    unsafe {
        std::env::remove_var("APTU_CODER_PROFILE");
    }
    let resp = call_tools_list_with_profile(None).await;

    // Act: extract tool count from response
    let tools = &resp["result"]["tools"];
    let tool_count = tools.as_array().map(|a| a.len()).unwrap_or(0);

    // Assert: no profile should enable all 7 tools (default behavior)
    assert_eq!(
        tool_count, 7,
        "no profile should enable all 7 tools (default behavior), got: {}",
        tool_count
    );
}

#[tokio::test]
async fn test_unknown_profile_tool_count() {
    // Arrange: hold guard across env var mutation and async call
    let _guard = env_var_lock().await;
    // Arrange: initialize with unknown profile string; env var must be absent.
    unsafe {
        std::env::remove_var("APTU_CODER_PROFILE");
    }
    let resp = call_tools_list_with_profile(Some("unknown_profile")).await;

    // Act: extract tool count from response
    let tools = &resp["result"]["tools"];
    let tool_count = tools.as_array().map(|a| a.len()).unwrap_or(0);

    // Assert: unknown profile should enable all 7 tools (lenient fallback)
    assert_eq!(
        tool_count, 7,
        "unknown profile should enable all 7 tools (lenient fallback), got: {}",
        tool_count
    );
}

#[tokio::test]
async fn test_disabled_tool_returns_invalid_params() {
    // Arrange: initialize with edit profile and try to call a disabled tool (analyze_directory)
    let resp = call_tool_with_profile(Some("edit"), "analyze_directory").await;

    // Act: extract error code from response
    let error_code = resp["error"]["code"].as_i64();

    // Assert: calling a disabled tool should return INVALID_PARAMS (-32602)
    assert_eq!(
        error_code,
        Some(-32602),
        "calling a disabled tool should return INVALID_PARAMS (-32602), got: {:?}",
        resp
    );
}

#[tokio::test]
async fn test_profile_env_var_fallback() {
    // Arrange: hold guard across env var mutation, async call, and cleanup
    let _guard = env_var_lock().await;
    // Arrange: set APTU_CODER_PROFILE env var to "edit", initialize with no _meta.
    unsafe {
        std::env::set_var("APTU_CODER_PROFILE", "edit");
    }

    let resp = call_tools_list_with_profile(None).await;

    // Cleanup after async call so the env var is present during initialize.
    unsafe {
        std::env::remove_var("APTU_CODER_PROFILE");
    }

    // Act: extract tool count from response.
    let tools = &resp["result"]["tools"];
    let tool_count = tools.as_array().map(|a| a.len()).unwrap_or(0);
    let tool_names: Vec<String> = tools
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
        .collect();

    // Assert: env var profile should be applied when _meta is absent.
    assert_eq!(
        tool_count, 3,
        "env var profile (edit) should enable exactly 3 tools, got: {:?}",
        tool_names
    );
    assert!(
        tool_names.contains(&"edit_replace".to_string()),
        "env var profile should include edit_replace"
    );
}

#[tokio::test]
async fn test_profile_env_var_ignored_when_meta_present() {
    // Arrange: hold guard across env var mutation, async call, and cleanup
    let _guard = env_var_lock().await;
    // Arrange: set APTU_CODER_PROFILE=edit but initialize with _meta "analyze".
    // _meta must win.
    unsafe {
        std::env::set_var("APTU_CODER_PROFILE", "edit");
    }

    let resp = call_tools_list_with_profile(Some("analyze")).await;

    // Cleanup after async call so the env var is present during initialize.
    unsafe {
        std::env::remove_var("APTU_CODER_PROFILE");
    }

    // Act: extract tool count from response.
    let tools = &resp["result"]["tools"];
    let tool_count = tools.as_array().map(|a| a.len()).unwrap_or(0);
    let tool_names: Vec<String> = tools
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
        .collect();

    // Assert: _meta profile (analyze) should override the env var (edit).
    // analyze profile enables 5 tools.
    assert_eq!(
        tool_count, 5,
        "_meta profile (analyze) should take precedence over env var, got: {:?}",
        tool_names
    );
    assert!(
        tool_names.contains(&"analyze_directory".to_string()),
        "_meta profile should include analyze_directory"
    );
}

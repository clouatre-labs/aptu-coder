// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::thread;
use std::time::Duration;

#[test]
fn test_mcp_server_responds_to_tools_call() {
    let bin = std::env::var("CARGO_BIN_EXE_aptu_coder").unwrap_or_else(|_| {
        // Fallback: construct path relative to workspace root
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .expect("manifest dir has grandparent (crates/<name>)");
        workspace_root
            .join("target/debug/aptu-coder")
            .to_string_lossy()
            .to_string()
    });

    let mut child = std::process::Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn server at {}: {}", bin, e));

    let mut stdin = child.stdin.take().expect("failed to get stdin");

    // Send initialize message
    let init_msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
    stdin
        .write_all(init_msg.as_bytes())
        .expect("failed to write");
    stdin.write_all(b"\n").expect("failed to write newline");

    // Send initialized notification
    let init_notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
    stdin
        .write_all(init_notif.as_bytes())
        .expect("failed to write");
    stdin.write_all(b"\n").expect("failed to write newline");

    // Send tool call
    let tool_call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"analyze_directory","arguments":{"path":"src","max_depth":1,"page_size":100,"summary":true}}}"#;
    stdin
        .write_all(tool_call.as_bytes())
        .expect("failed to write");
    stdin.write_all(b"\n").expect("failed to write newline");

    drop(stdin); // Close stdin to signal end of input

    let stdout = child.stdout.take().expect("failed to get stdout");
    let reader = BufReader::new(stdout);

    let (tx, rx) = std::sync::mpsc::channel();
    let reader_thread = thread::spawn(move || {
        for line in reader.lines() {
            if let Ok(line) = line {
                let _ = tx.send(line);
            }
        }
    });

    // Wait up to 5 seconds for responses
    let timeout = Duration::from_secs(5);
    let start = std::time::Instant::now();
    let mut found_valid_response = false;

    while start.elapsed() < timeout {
        match rx.try_recv() {
            Ok(line) => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                    // Check if this is a valid JSON-RPC 2.0 response with result
                    if json.get("result").is_some() && json.get("error").is_none() {
                        found_valid_response = true;
                        break;
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                break;
            }
        }
    }

    // Wait for child to exit cleanly
    let _ = child.wait();
    let _ = reader_thread.join();

    assert!(
        found_valid_response,
        "Expected at least one valid JSON-RPC 2.0 response with result field and no error field"
    );
}

#[test]
fn test_mcp_server_recovers_after_tool_error() {
    let bin = std::env::var("CARGO_BIN_EXE_aptu_coder").unwrap_or_else(|_| {
        // Fallback: construct path relative to workspace root
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .expect("manifest dir has grandparent (crates/<name>)");
        workspace_root
            .join("target/debug/aptu-coder")
            .to_string_lossy()
            .to_string()
    });

    let mut child = std::process::Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn server at {}: {}", bin, e));

    let mut stdin = child.stdin.take().expect("failed to get stdin");

    // Writer thread: pace messages to avoid EOF race with the server's async reader.
    let writer = thread::spawn(move || {
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
        stdin.write_all(init.as_bytes()).expect("write init");
        stdin.write_all(b"\n").expect("newline");

        let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        stdin.write_all(notif.as_bytes()).expect("write notif");
        stdin.write_all(b"\n").expect("newline");

        thread::sleep(Duration::from_millis(500));

        // Tool call with a nonexistent path — must return isError=true, not crash.
        let bad = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"analyze_file","arguments":{"path":"/nonexistent/does_not_exist.py","ast_recursion_limit":null,"page_size":null}}}"#;
        stdin.write_all(bad.as_bytes()).expect("write bad call");
        stdin.write_all(b"\n").expect("newline");

        thread::sleep(Duration::from_millis(2000));

        // Follow-up call — server must still be alive.
        let good = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"analyze_directory","arguments":{"path":"src","max_depth":1,"page_size":100,"summary":true}}}"#;
        stdin.write_all(good.as_bytes()).expect("write good call");
        stdin.write_all(b"\n").expect("newline");

        thread::sleep(Duration::from_millis(3000));
        // stdin dropped here, server will exit cleanly
    });

    let stdout = child.stdout.take().expect("failed to get stdout");
    let reader = std::io::BufReader::new(stdout);
    let (tx, rx) = std::sync::mpsc::channel();
    let reader_thread = thread::spawn(move || {
        use std::io::BufRead;
        for line in reader.lines().flatten() {
            let _ = tx.send(line);
        }
    });

    let timeout = Duration::from_secs(12);
    let start = std::time::Instant::now();
    let mut got_error_response = false;
    let mut got_recovery_response = false;

    while start.elapsed() < timeout && !(got_error_response && got_recovery_response) {
        match rx.try_recv() {
            Ok(line) => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                    if json.get("id") == Some(&serde_json::json!(2)) {
                        assert!(
                            json.get("error").is_none(),
                            "id:2 must not be a JSON-RPC protocol error: {}",
                            line
                        );
                        assert!(
                            json.get("result").is_some(),
                            "id:2 must have result field: {}",
                            line
                        );
                        let is_error = json["result"]["isError"].as_bool().unwrap_or(false);
                        assert!(is_error, "id:2 result must have isError=true: {}", line);
                        got_error_response = true;
                    }
                    if json.get("id") == Some(&serde_json::json!(3)) {
                        assert!(
                            json.get("result").is_some(),
                            "id:3 must have result field (server must be alive after tool error): {}",
                            line
                        );
                        got_recovery_response = true;
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
    }

    let _ = writer.join();
    let _ = child.wait();
    let _ = reader_thread.join();

    assert!(
        got_error_response,
        "Did not receive a result for id:2 (tool error call)"
    );
    assert!(
        got_recovery_response,
        "Server did not respond to id:3 after a tool error (transport closed)"
    );
}

#[test]
fn test_mcp_server_exec_command() {
    let bin = std::env::var("CARGO_BIN_EXE_aptu_coder").unwrap_or_else(|_| {
        // Fallback: construct path relative to workspace root
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .expect("manifest dir has grandparent (crates/<name>)");
        workspace_root
            .join("target/debug/aptu-coder")
            .to_string_lossy()
            .to_string()
    });

    let mut child = std::process::Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn server at {}: {}", bin, e));

    let mut stdin = child.stdin.take().expect("failed to get stdin");

    // Writer thread: pace messages to avoid EOF race with the server's async reader.
    let writer = thread::spawn(move || {
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
        stdin.write_all(init.as_bytes()).expect("write init");
        stdin.write_all(b"\n").expect("newline");

        let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        stdin.write_all(notif.as_bytes()).expect("write notif");
        stdin.write_all(b"\n").expect("newline");

        thread::sleep(Duration::from_millis(200));

        // Tool call: exec_command with echo
        let tool_call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"exec_command","arguments":{"command":"echo smoke_test"}}}"#;
        stdin
            .write_all(tool_call.as_bytes())
            .expect("write tool call");
        stdin.write_all(b"\n").expect("newline");

        thread::sleep(Duration::from_millis(2000));
        // stdin dropped here, server will exit cleanly
    });

    let stdout = child.stdout.take().expect("failed to get stdout");
    let reader = BufReader::new(stdout);
    let (tx, rx) = std::sync::mpsc::channel();
    let reader_thread = thread::spawn(move || {
        for line in reader.lines().flatten() {
            let _ = tx.send(line);
        }
    });

    let timeout = Duration::from_secs(10);
    let start = std::time::Instant::now();
    let mut found_exec_command_response = false;

    while start.elapsed() < timeout {
        match rx.try_recv() {
            Ok(line) => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                    // Check for response with id=2 (our exec_command call)
                    if json.get("id") == Some(&serde_json::json!(2)) {
                        // Must have result field, not error
                        assert!(
                            json.get("result").is_some(),
                            "id:2 must have result field: {}",
                            line
                        );
                        assert!(
                            json.get("error").is_none(),
                            "id:2 must not have error field: {}",
                            line
                        );
                        // Check that isError is false or absent
                        let is_error = json["result"]["isError"].as_bool().unwrap_or(false);
                        assert!(
                            !is_error,
                            "id:2 result must have isError=false or absent: {}",
                            line
                        );
                        // Optionally verify content contains smoke_test or exit_code is 0
                        let content = json["result"]["content"].as_array();
                        if let Some(content_arr) = content {
                            if let Some(first) = content_arr.first() {
                                if let Some(text) = first.get("text") {
                                    let text_str = text.as_str().unwrap_or("");
                                    assert!(
                                        text_str.contains("smoke_test")
                                            || text_str.contains("exit_code")
                                            || text_str.contains("0"),
                                        "content should contain smoke_test or exit info: {}",
                                        text_str
                                    );
                                }
                            }
                        }
                        found_exec_command_response = true;
                        break;
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
    }

    let _ = writer.join();
    let _ = child.wait();
    let _ = reader_thread.join();

    assert!(
        found_exec_command_response,
        "Expected valid response for exec_command tool call with id=2"
    );
}

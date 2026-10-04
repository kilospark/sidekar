use super::*;

fn dummy_server() -> Server {
    Server {
        bus_name: "sidekar-mcp-test-1".to_string(),
        inbox: Arc::new(Mutex::new(Vec::new())),
        exe: PathBuf::from("sidekar"),
    }
}

#[test]
fn blocked_commands_are_interactive_or_destructive_only() {
    for cmd in ["repl", "daemon", "session", "mcp", "device", "install", "uninstall", "update"] {
        assert!(command_blocked(cmd).is_some(), "{cmd} should be blocked");
    }
    for cmd in ["bus", "kv", "totp", "hotp", "duo", "memory", "tasks", "navigate"] {
        assert!(command_blocked(cmd).is_none(), "{cmd} should be allowed");
    }
}

#[test]
fn the_tool_description_lists_real_commands_and_hides_blocked_ones() {
    let desc = sidekar_tool_description();
    assert!(desc.contains("bus"), "lists the bus command");
    assert!(desc.contains("kv"), "lists the kv command");
    // Blocked commands must not be advertised.
    assert!(!desc.contains("repl"), "repl is blocked, not advertised");
    assert!(!desc.contains("uninstall"), "uninstall is blocked, not advertised");
    // Group headings come from the catalog.
    assert!(desc.contains("Account:"), "groups commands by catalog group");
}

#[test]
fn the_tool_list_has_both_tools_with_their_schemas() {
    let tools = tool_list();
    let arr = tools.as_array().unwrap();
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, ["sidekar", "bus_inbox"]);
    let sidekar = &arr[0];
    assert_eq!(
        sidekar["inputSchema"]["required"],
        json!(["args"]),
        "the generic tool requires args"
    );
    assert_eq!(
        sidekar["inputSchema"]["properties"]["args"]["type"],
        "array"
    );
}

#[test]
fn jsonrpc_envelopes_carry_the_id_and_version() {
    let ok = ok_response(Some(json!(7)), json!({ "x": 1 }));
    assert_eq!(ok["jsonrpc"], "2.0");
    assert_eq!(ok["id"], 7);
    assert_eq!(ok["result"]["x"], 1);

    let err = err_response(Some(json!("a")), -32601, "nope");
    assert_eq!(err["id"], "a");
    assert_eq!(err["error"]["code"], -32601);
    assert_eq!(err["error"]["message"], "nope");

    // A request with no id (notification) still renders id: null if forced.
    assert_eq!(ok_response(None, json!({}))["id"], Value::Null);
}

#[tokio::test]
async fn initialize_echoes_the_clients_protocol_version_and_sends_instructions() {
    let srv = dummy_server();
    let req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-03-26" }
    });
    let resp = handle_request(&srv, &req).await.unwrap();
    assert_eq!(resp["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(resp["result"]["serverInfo"]["name"], "sidekar");
    assert_eq!(resp["result"]["capabilities"]["tools"], json!({}));
    // Instructions are the embedded SKILL.md — a single source of docs.
    let instructions = resp["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("Sidekar"), "ships SKILL.md as instructions");
}

#[tokio::test]
async fn initialize_falls_back_to_the_default_version() {
    let srv = dummy_server();
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" });
    let resp = handle_request(&srv, &req).await.unwrap();
    assert_eq!(resp["result"]["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
}

#[tokio::test]
async fn tools_list_returns_the_catalog() {
    let srv = dummy_server();
    let req = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let resp = handle_request(&srv, &req).await.unwrap();
    assert_eq!(resp["result"]["tools"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn notifications_get_no_response() {
    let srv = dummy_server();
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert!(handle_request(&srv, &note).await.is_none());
}

#[tokio::test]
async fn ping_is_answered_with_an_empty_result() {
    let srv = dummy_server();
    let req = json!({ "jsonrpc": "2.0", "id": 9, "method": "ping" });
    let resp = handle_request(&srv, &req).await.unwrap();
    assert_eq!(resp["result"], json!({}));
}

#[tokio::test]
async fn bus_inbox_drains_buffered_messages_once() {
    let srv = dummy_server();
    srv.inbox
        .lock()
        .unwrap()
        .extend(["From alice:\nhi".to_string(), "From bob:\nyo".to_string()]);

    let call = json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "bus_inbox", "arguments": {} }
    });
    let resp = handle_request(&srv, &call).await.unwrap();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("alice") && text.contains("bob"));
    assert_eq!(resp["result"]["isError"], false);

    // Draining clears it: a second read is empty.
    let resp2 = handle_request(&srv, &call).await.unwrap();
    assert_eq!(
        resp2["result"]["content"][0]["text"],
        "No new bus messages."
    );
}

#[tokio::test]
async fn a_blocked_command_is_a_tool_error_and_never_spawns() {
    let srv = dummy_server();
    let call = json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": { "name": "sidekar", "arguments": { "args": ["repl"] } }
    });
    let resp = handle_request(&srv, &call).await.unwrap();
    assert_eq!(resp["result"]["isError"], true);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("not available over MCP"), "{text}");
}

#[test]
fn help_is_allowed_even_though_it_is_not_a_catalog_command() {
    // The tool description tells the model to run ["help","<cmd>"]; the runner
    // must not reject it as unknown. (main.rs handles `help` before the table.)
    assert!(!crate::is_known_command("help"), "help is not in the catalog");
    assert!(command_blocked("help").is_none(), "help must not be blocked");
}

#[tokio::test]
async fn an_unknown_command_is_a_tool_error() {
    let srv = dummy_server();
    let call = json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "sidekar", "arguments": { "args": ["definitely-not-a-command"] } }
    });
    let resp = handle_request(&srv, &call).await.unwrap();
    assert_eq!(resp["result"]["isError"], true);
    assert!(
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Unknown sidekar command")
    );
}

#[tokio::test]
async fn an_unknown_tool_is_a_jsonrpc_error() {
    let srv = dummy_server();
    let call = json!({
        "jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": { "name": "bogus", "arguments": {} }
    });
    let resp = handle_request(&srv, &call).await.unwrap();
    assert_eq!(resp["error"]["code"], -32602);
}

#[test]
fn the_inbox_note_is_appended_only_when_messages_wait() {
    let srv = dummy_server();
    assert_eq!(srv.with_inbox_note("done".to_string()), "done");
    srv.inbox.lock().unwrap().push("From x:\nhi".to_string());
    let noted = srv.with_inbox_note("done".to_string());
    assert!(noted.starts_with("done\n"));
    assert!(noted.contains("1 new bus message waiting"));
}

#[test]
fn upsert_adds_is_idempotent_and_preserves_other_keys() {
    // Start from a realistic Claude Desktop config with unrelated keys.
    let mut root = json!({
        "coworkUserFilesPath": "/Users/me/Claude",
        "preferences": { "menuBarEnabled": false },
        "mcpServers": { "other": { "command": "x", "args": [] } }
    });

    assert!(upsert_mcp_server(&mut root, "sidekar", "/usr/local/bin/sidekar"));
    // Unrelated keys survive.
    assert_eq!(root["coworkUserFilesPath"], "/Users/me/Claude");
    assert_eq!(root["preferences"]["menuBarEnabled"], false);
    assert_eq!(root["mcpServers"]["other"]["command"], "x");
    // Ours is correct.
    assert_eq!(root["mcpServers"]["sidekar"]["command"], "/usr/local/bin/sidekar");
    assert_eq!(root["mcpServers"]["sidekar"]["args"], json!(["mcp"]));
    // Running it again is a no-op.
    assert!(!upsert_mcp_server(&mut root, "sidekar", "/usr/local/bin/sidekar"));
    // A changed path updates.
    assert!(upsert_mcp_server(&mut root, "sidekar", "/opt/sidekar"));
    assert_eq!(root["mcpServers"]["sidekar"]["command"], "/opt/sidekar");
}

#[test]
fn upsert_builds_the_structure_from_an_empty_object() {
    let mut root = json!({});
    assert!(upsert_mcp_server(&mut root, "sidekar", "/bin/sidekar"));
    assert_eq!(root["mcpServers"]["sidekar"]["args"], json!(["mcp"]));
}

#[test]
fn upsert_repairs_a_non_object_mcpservers() {
    // A malformed config shouldn't crash install.
    let mut root = json!({ "mcpServers": "oops" });
    assert!(upsert_mcp_server(&mut root, "sidekar", "/bin/sidekar"));
    assert!(root["mcpServers"].is_object());
    assert_eq!(root["mcpServers"]["sidekar"]["command"], "/bin/sidekar");
}

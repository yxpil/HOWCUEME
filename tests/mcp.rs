//! Integration tests for the MCP surface of `howcueme serve` (Streamable
//! HTTP JSON-RPC on `/` and `/mcp`). Runs the real binary with an isolated
//! data dir and a never-firing rule, exercising status/list/fire/validate,
//! protocol errors and the forced-trigger path end to end.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

struct ServeHandle {
    child: Child,
    addr: String,
}

impl Drop for ServeHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("howcueme-mcp-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Command action that is safe on every platform.
#[cfg(unix)]
fn command_action(marker: &Path) -> String {
    format!(
        "type = \"command\"\ncmd = \"touch\"\nargs = ['{}']",
        marker.display()
    )
}

#[cfg(windows)]
fn command_action(marker: &Path) -> String {
    format!(
        "type = \"command\"\ncmd = \"cmd\"\nargs = ['/C', 'copy NUL {}']",
        marker.display()
    )
}

/// Data dir with a rule whose process condition never matches (the internal
/// poller stays quiet) plus a command action writing a marker file when fired.
fn setup(tag: &str) -> PathBuf {
    let dir = unique_temp_dir(tag);
    let marker = dir.join("fired.txt");
    fs::write(
        dir.join("rules.toml"),
        format!(
            "[[rule]]\nname = \"serve-fire\"\n[rule.when]\ntype = \"process\"\nname = \"howcueme-no-such-proc-xyz\"\nop = \"exists\"\n[rule.action]\n{}\n",
            command_action(&marker)
        ),
    )
    .unwrap();
    dir
}

fn spawn_serve(dir: &Path) -> ServeHandle {
    let mut child = Command::new(env!("CARGO_BIN_EXE_howcueme"))
        .args(["serve", "--port", "0"])
        .env("HOWCUEME_DATA_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn howcueme serve");
    let stderr = child.stderr.take().expect("serve stderr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if let Some(addr) = line
                .trim()
                .strip_prefix("howcueme serve listening on http://")
                .and_then(|rest| rest.split_whitespace().next())
            {
                let _ = tx.send(addr.to_string());
                break;
            }
        }
    });
    let addr = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("serve did not print its listening address in time");
    ServeHandle { child, addr }
}

/// POST a JSON-RPC message; returns (status, Mcp-Session-Id?, body).
fn rpc(addr: &str, session: Option<&str>, message: Value) -> (u16, Option<String>, Value) {
    let mut request = ureq::post(&format!("http://{addr}/mcp")).timeout(Duration::from_secs(30));
    if let Some(sid) = session {
        request = request.set("Mcp-Session-Id", sid);
    }
    match request.send_string(&message.to_string()) {
        Ok(resp) => (
            resp.status(),
            resp.header("Mcp-Session-Id").map(str::to_string),
            resp.into_json().unwrap_or(Value::Null),
        ),
        Err(ureq::Error::Status(status, resp)) => (
            status,
            resp.header("Mcp-Session-Id").map(str::to_string),
            resp.into_json().unwrap_or(Value::Null),
        ),
        Err(e) => panic!("http request failed: {e}"),
    }
}

fn initialize(addr: &str) -> String {
    let (status, sid, body) = rpc(
        addr,
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }),
    );
    assert_eq!(status, 200);
    assert_eq!(body["result"]["serverInfo"]["name"], "howcueme");
    assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
    sid.expect("initialize must issue an Mcp-Session-Id")
}

fn call(addr: &str, sid: &str, id: i64, name: &str, args: Value) -> Value {
    let (_, _, body) = rpc(
        addr,
        Some(sid),
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }),
    );
    body["result"].clone()
}

fn payload(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().expect("text content"))
        .expect("tool payload is JSON")
}

#[test]
fn handshake_tools_list_and_full_status_list_fire_validate_chain() {
    let dir = setup("chain");
    let marker = dir.join("fired.txt");
    let server = spawn_serve(&dir);
    let addr = server.addr.clone();
    let sid = initialize(&addr);

    // Notifications are accepted silently.
    let (status, _, body) = rpc(
        &addr,
        Some(&sid),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );
    assert_eq!(status, 202);
    assert_eq!(body, Value::Null);

    // tools/list: four tools with schemas.
    let (_, _, body) = rpc(
        &addr,
        Some(&sid),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    );
    let tools = body["result"]["tools"].as_array().expect("tools");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, vec!["status", "list", "fire", "validate"]);
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(!tool["description"].as_str().unwrap().is_empty());
    }
    let fire = tools.iter().find(|t| t["name"] == "fire").unwrap();
    assert_eq!(fire["inputSchema"]["required"], json!(["rule"]));

    // status
    let result = call(&addr, &sid, 3, "status", json!({}));
    assert_eq!(result["isError"], false);
    let v = payload(&result);
    assert_eq!(v["ok"], true);
    assert_eq!(v["status"]["rules"], 1);
    assert_eq!(v["status"]["enabled_rules"], 1);
    assert!(v["status"]["uptime_secs"].is_u64());

    // list
    let result = call(&addr, &sid, 4, "list", json!({}));
    assert_eq!(result["isError"], false);
    let v = payload(&result);
    assert_eq!(v["rules"][0]["name"], "serve-fire");

    // fire (forced): the command action writes the marker file
    assert!(!marker.exists(), "marker must not exist before firing");
    let result = call(&addr, &sid, 5, "fire", json!({"rule": "serve-fire"}));
    assert_eq!(result["isError"], false, "{}", result);
    let v = payload(&result);
    assert_eq!(v["ok"], true);
    assert_eq!(v["event"]["rule"], "serve-fire");
    assert_eq!(v["event"]["forced"], true);
    assert_eq!(v["event"]["result"]["exit_code"], 0);
    assert!(marker.exists(), "command action must have run");

    // fire with an unknown rule -> isError naming the rule
    let result = call(&addr, &sid, 6, "fire", json!({"rule": "missing-rule"}));
    assert_eq!(result["isError"], true);
    assert!(result["content"][0]["text"]
        .as_str()
        .expect("text")
        .contains("rule not found"));

    // fire without a rule -> isError explaining the requirement
    let result = call(&addr, &sid, 7, "fire", json!({}));
    assert_eq!(result["isError"], true);
    assert!(result["content"][0]["text"]
        .as_str()
        .expect("text")
        .contains("params.rule is required"));

    // list reflects the updated last_triggered
    let result = call(&addr, &sid, 8, "list", json!({}));
    assert_eq!(result["isError"], false);
    let v = payload(&result);
    assert!(v["rules"][0]["last_triggered_secs"].as_i64().unwrap() > 0);

    // validate (rules file is well-formed)
    let result = call(&addr, &sid, 9, "validate", json!({}));
    assert_eq!(result["isError"], false);
    let v = payload(&result);
    assert_eq!(v["ok"], true);
    assert_eq!(v["validate"]["valid"], true);
    assert_eq!(v["validate"]["errors"], json!([]));
}

#[test]
fn unknown_tool_and_protocol_errors() {
    let dir = setup("errors");
    let server = spawn_serve(&dir);
    let addr = server.addr.clone();
    let sid = initialize(&addr);

    // Unknown tool -> JSON-RPC -32602.
    let (_, _, body) = rpc(
        &addr,
        Some(&sid),
        json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": { "name": "kill_daemon", "arguments": {} }
        }),
    );
    assert_eq!(body["error"]["code"], -32602);

    // Unknown method -> -32601; ping -> {}.
    let (_, _, body) = rpc(
        &addr,
        Some(&sid),
        json!({ "jsonrpc": "2.0", "id": 11, "method": "resources/list" }),
    );
    assert_eq!(body["error"]["code"], -32601);
    let (_, _, body) = rpc(
        &addr,
        Some(&sid),
        json!({ "jsonrpc": "2.0", "id": 12, "method": "ping" }),
    );
    assert_eq!(body["result"], json!({}));

    // Parameterless tools tolerate junk arguments.
    let result = call(&addr, &sid, 13, "status", json!({"bogus": true}));
    assert_eq!(result["isError"], false);
}

#[test]
fn root_endpoint_serves_mcp_too() {
    let dir = setup("root");
    let server = spawn_serve(&dir);
    // BIT discovery probes the root: initialize must work on POST / as well.
    let response = ureq::post(&format!("http://{}/", server.addr))
        .timeout(Duration::from_secs(5))
        .set("Content-Type", "application/json")
        .send_string(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{}}}"#,
        )
        .expect("root request succeeds");
    assert_eq!(response.status(), 200);
    let body: Value = response.into_json().expect("json");
    assert_eq!(body["result"]["serverInfo"]["name"], "howcueme");
}

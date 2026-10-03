//! 不可信输入注入安全测试 —— howcueme 的 HTTP/MCP 表面吃外部喂进来的 JSON。
//!
//! 攻击面：`POST /invoke`、`POST /mcp`（JSON-RPC）、`fire` 的规则名。
//! 断言：畸形/恶意载荷被拒绝、不打 shell、错误始终是合法 JSON；
//! 路径穿越/命令注入风格的规则名只按名字查注册表，绝不执行。

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

struct Server {
    child: Child,
    addr: String,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("howcueme-inj-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn setup() -> PathBuf {
    let dir = tmp("svr");
    // 一个永远不会触发的 process 条件规则，保证内部 poller 安静。
    fs::write(
        dir.join("rules.toml"),
        "[[rule]]\nname = \"peace\"\n[rule.when]\ntype = \"process\"\nname = \"howcueme-no-such-proc-xyz\"\nop = \"exists\"\n[rule.action]\ntype = \"command\"\ncmd = \"echo\"\nargs = []\n",
    )
    .unwrap();
    dir
}

fn spawn(dir: &PathBuf) -> Server {
    let mut child = Command::new(env!("CARGO_BIN_EXE_howcueme"))
        .args(["serve", "--port", "0"])
        .env("HOWCUEME_DATA_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn howcueme serve");
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if let Some(addr) = line
                .trim()
                .strip_prefix("howcueme serve listening on http://")
                .and_then(|r| r.split_whitespace().next())
            {
                let _ = tx.send(addr.to_string());
                break;
            }
        }
    });
    let addr = rx.recv_timeout(Duration::from_secs(20)).expect("serve up");
    Server { child, addr }
}

fn rpc(addr: &str, msg: Value) -> (u16, Value) {
    match ureq::post(&format!("http://{addr}/mcp"))
        .timeout(Duration::from_secs(20))
        .send_string(&msg.to_string())
    {
        Ok(r) => (r.status(), r.into_json().unwrap_or(Value::Null)),
        Err(ureq::Error::Status(c, r)) => (c, r.into_json().unwrap_or(Value::Null)),
        Err(e) => panic!("rpc failed: {e}"),
    }
}

#[test]
fn malformed_json_to_mcp_is_parse_error_not_crash() {
    let srv = spawn(&setup());
    // 直接发裸垃圾字节 → JSON-RPC 解析错误 -32700，服务不崩
    match ureq::post(&format!("http://{}/mcp", srv.addr))
        .timeout(Duration::from_secs(5))
        .send_string("{{{ not json")
    {
        Ok(r) => {
            let v: Value = r.into_json().unwrap();
            assert_eq!(v["error"]["code"], -32700, "malformed JSON must be -32700: {v}");
        }
        other => panic!("malformed body must get a structured JSON-RPC error, got {other:?}"),
    }
}

#[test]
fn xss_tool_name_is_safely_embedded_in_json_error() {
    let srv = spawn(&setup());
    let raw = json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"<script>alert(document.cookie)</script>","arguments":{}}
    })
    .to_string();
    let (_status, body) = rpc(&srv.addr, serde_json::from_str(&raw).unwrap());
    assert_eq!(body["error"]["code"], -32602, "{body}");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("alert"), "工具名应作为数据回显: {msg}");
}

#[test]
fn fire_with_traversal_or_command_string_name_is_not_found() {
    let srv = spawn(&setup());
    // 规则名里塞路径穿越 / 命令注入：只按名字查注册表 → not found，绝不执行。
    for evil in ["../../etc/passwd", "peace; rm -rf /", "x`whoami`"] {
        let (_, body) = rpc(
            &srv.addr,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                   "params":{"name":"fire","arguments":{"rule": evil}}}),
        );
        let result = &body["result"];
        assert_eq!(result["isError"], true, "恶意规则名应触发 not-found 错误: evil={evil} body={body}");
    }
}

#[test]
fn unknown_action_to_invoke_is_400() {
    let srv = spawn(&setup());
    match ureq::post(&format!("http://{}/invoke", srv.addr))
        .timeout(Duration::from_secs(5))
        .send_json(json!({"params":{"action":"scan; reboot"}}))
    {
        Err(ureq::Error::Status(400, resp)) => {
            let v: Value = resp.into_json().unwrap();
            assert_eq!(v["ok"], false);
        }
        other => panic!("unknown action must be 400, got {other:?}"),
    }
}

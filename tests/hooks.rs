//! 钩子/规则触发测试 —— howcueme 把"规则(rule)"当钩子：注册时给一个名字 + 触发条件 + 动作，
//! 运行时 `fire` 按名触发。这里断言：
//! 1. 已注册工具按稳定顺序列出、按名触发；
//! 2. 未注册工具/规则被拒绝（-32602 / isError）；
//! 3. 一次失败的触发不影响兄弟工具（失败隔离）。

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
    let dir =
        std::env::temp_dir().join(format!("howcueme-hooks-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn setup() -> PathBuf {
    let dir = tmp("svr");
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

fn rpc(addr: &str, msg: Value) -> Value {
    match ureq::post(&format!("http://{addr}/mcp"))
        .timeout(Duration::from_secs(20))
        .send_string(&msg.to_string())
    {
        Ok(r) => r.into_json().unwrap_or(Value::Null),
        Err(ureq::Error::Status(_, r)) => r.into_json().unwrap_or(Value::Null),
        Err(e) => panic!("rpc failed: {e}"),
    }
}

fn initialize(addr: &str) {
    let body = rpc(
        addr,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize",
               "params":{"protocolVersion":"2025-03-26","capabilities":{}}}),
    );
    assert_eq!(body["result"]["serverInfo"]["name"], "howcueme");
}

#[test]
fn registered_tools_listed_in_stable_order_and_each_routes() {
    let srv = spawn(&setup());
    initialize(&srv.addr);
    let body = rpc(
        &srv.addr,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["status", "list", "fire", "validate"]);

    // 每个已注册工具都能按名触发
    let body = rpc(
        &srv.addr,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
               "params":{"name":"status","arguments":{}}}),
    );
    assert_eq!(body["result"]["isError"], false, "{body}");
}

#[test]
fn unregistered_tool_is_rejected() {
    let srv = spawn(&setup());
    initialize(&srv.addr);
    let body = rpc(
        &srv.addr,
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
               "params":{"name":"delete_everything","arguments":{}}}),
    );
    assert_eq!(body["error"]["code"], -32602, "未注册工具必须拒绝: {body}");
}

#[test]
fn failed_trigger_does_not_break_sibling_tools() {
    let srv = spawn(&setup());
    initialize(&srv.addr);

    // 先触发一个不存在的规则（fire 报错，失败隔离）
    let body = rpc(
        &srv.addr,
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
               "params":{"name":"fire","arguments":{"rule":"no-such-rule"}}}),
    );
    assert_eq!(body["result"]["isError"], true, "不存在的规则应失败: {body}");

    // 紧接着兄弟工具 status / list 照常可用
    let body = rpc(
        &srv.addr,
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call",
               "params":{"name":"status","arguments":{}}}),
    );
    assert_eq!(body["result"]["isError"], false, "兄弟工具应仍可用: {body}");
    let body = rpc(
        &srv.addr,
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
               "params":{"name":"list","arguments":{}}}),
    );
    assert_eq!(body["result"]["isError"], false, "list 应仍可用: {body}");
}

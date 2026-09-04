//! MCP (Model Context Protocol) server over Streamable HTTP — hand-rolled
//! JSON-RPC 2.0 on axum, wire-compatible with BIT's MCP client (the same
//! contract SECFORGE, PANOPTES, Neton and MemoryPool speak). Tool calls route
//! into the shared action funnel in `serve::dispatch_action`, so the MCP
//! surface and `POST /invoke` always agree.
//!
//! Contract (verified against BIT's client):
//! - `initialize` → result `{protocolVersion, capabilities:{tools:{listChanged:false}}, serverInfo}`
//!   plus an `Mcp-Session-Id` response header (echoed back by clients).
//! - `notifications/*` (or any id-less message) → HTTP 202, empty body.
//! - `tools/list` → `{tools:[{name, description, inputSchema}]}` (single page).
//! - `tools/call` → `{content:[{type:"text", text:<json string>}], isError}` —
//!   failures are 200 + `isError:true`, never transport errors.
//! - unknown method → JSON-RPC error -32601; `ping` → empty result.
//!
//! Sessions are issued for spec compliance but not tracked server-side.

use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::poll::PollCtx;
use crate::serve::{dispatch_action, ActionError};

/// Protocol versions we can speak; we echo the client's choice when possible.
const MCP_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

static SESSION_SEQ: AtomicU64 = AtomicU64::new(0);

/// One howcueme capability exposed over MCP.
pub struct ToolDef {
    /// MCP tool name = the action name.
    pub name: &'static str,
    /// English description surfaced in `tools/list`.
    pub description: &'static str,
    /// JSON Schema for the `arguments` object.
    pub input_schema: Value,
}

/// Build a JSON-Schema object from `(name, type, description, required)` tuples.
fn schema(fields: &[(&str, &str, &str, bool)]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, ty, desc, req) in fields {
        properties.insert(
            (*name).to_string(),
            json!({ "type": ty, "description": desc }),
        );
        if *req {
            required.push(json!(name));
        }
    }
    let mut schema = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        schema["required"] = Value::Array(required);
    }
    schema
}

/// All tools howcueme exposes, in stable order.
pub fn tools() -> &'static [ToolDef] {
    static TOOLS: std::sync::OnceLock<Vec<ToolDef>> = std::sync::OnceLock::new();
    TOOLS.get_or_init(|| {
        vec![
            ToolDef {
                name: "status",
                description: "Daemon overview: rule counts, enabled rules, rules file path, \
                              data dir, poll interval and uptime.",
                input_schema: schema(&[]),
            },
            ToolDef {
                name: "list",
                description: "List every rule with its condition, action and last-trigger info.",
                input_schema: schema(&[]),
            },
            ToolDef {
                name: "fire",
                description: "Force-trigger one rule right now, regardless of its condition. \
                              Returns the triggered event (action outcome included).",
                input_schema: schema(&[(
                    "rule",
                    "string",
                    "Rule name to force-trigger (alias: name)",
                    true,
                )]),
            },
            ToolDef {
                name: "validate",
                description: "Re-validate the rules file and report parse/validation errors.",
                input_schema: schema(&[]),
            },
        ]
    })
}

/// Mirror BIT's `gen_mcp_session_id`: monotonic, unique per process.
fn gen_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("mcp-{nanos:x}-{:x}-{seq:x}", std::process::id())
}

fn negotiate_version(client: &str) -> &'static str {
    MCP_VERSIONS
        .iter()
        .find(|v| **v == client)
        .copied()
        .unwrap_or(MCP_VERSIONS[MCP_VERSIONS.len() - 1])
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_err(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn json_response(status: StatusCode, body: Option<Value>, session: Option<&str>) -> Response {
    let mut builder = Response::builder().status(status);
    if let Some(sid) = session {
        builder = builder.header("Mcp-Session-Id", sid);
    }
    match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(axum::body::Body::from(v.to_string()))
            .expect("static response"),
        None => builder
            .body(axum::body::Body::empty())
            .expect("static response"),
    }
    .into_response()
}

/// MCP JSON-RPC entry point, mounted on both `/` (BIT discovery probes the root)
/// and `/mcp` (the canonical Streamable HTTP path).
async fn rpc_entry(State(ctx): State<Arc<PollCtx>>, body: Bytes) -> Response {
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_response(
                StatusCode::OK,
                Some(rpc_err(Value::Null, -32700, &format!("parse error: {e}"))),
                None,
            );
        }
    };

    let method = msg
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = msg.get("id").is_none() || method.starts_with("notifications/");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    if is_notification {
        return json_response(StatusCode::ACCEPTED, None, None);
    }

    match method.as_str() {
        "initialize" => {
            let client_ver = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let session = gen_session_id();
            let result = json!({
                "protocolVersion": negotiate_version(client_ver),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "howcueme",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            });
            json_response(StatusCode::OK, Some(rpc_ok(id, result)), Some(&session))
        }
        "ping" => json_response(StatusCode::OK, Some(rpc_ok(id, json!({}))), None),
        "tools/list" => {
            let tools: Vec<Value> = tools()
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    })
                })
                .collect();
            json_response(
                StatusCode::OK,
                Some(rpc_ok(id, json!({ "tools": tools }))),
                None,
            )
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if !tools().iter().any(|t| t.name == name) {
                return json_response(
                    StatusCode::OK,
                    Some(rpc_err(id, -32602, &format!("tool not found: '{name}'"))),
                    None,
                );
            }
            let outcome = dispatch_action(ctx.clone(), name, &args).await;
            let (text, is_error) = match outcome {
                Ok(value) => (value.to_string(), false),
                Err(ActionError::Bad(message))
                | Err(ActionError::NotFound(message))
                | Err(ActionError::Internal(message)) => (message, true),
            };
            json_response(
                StatusCode::OK,
                Some(rpc_ok(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }),
                )),
                None,
            )
        }
        other => json_response(
            StatusCode::OK,
            Some(rpc_err(id, -32601, &format!("method not found: '{other}'"))),
            None,
        ),
    }
}

/// MCP JSON-RPC routes to merge into the serve router: POST `/` and `/mcp`.
pub fn routes() -> Router<Arc<PollCtx>> {
    Router::new()
        .route("/", post(rpc_entry))
        .route("/mcp", post(rpc_entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_four_tools_with_schemas() {
        let tools = tools();
        assert_eq!(tools.len(), 4);
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["status", "list", "fire", "validate"]);
        for tool in tools {
            assert_eq!(tool.input_schema["type"], "object");
            assert!(!tool.description.is_empty());
        }
    }

    #[test]
    fn fire_requires_the_rule_argument() {
        let fire = tools().iter().find(|t| t.name == "fire").unwrap();
        assert_eq!(fire.input_schema["required"], json!(["rule"]));
    }

    #[test]
    fn negotiate_echoes_supported_client_versions() {
        assert_eq!(negotiate_version("2024-11-05"), "2024-11-05");
        assert_eq!(negotiate_version("2025-03-26"), "2025-03-26");
        assert_eq!(negotiate_version("1999-01-01"), "2025-06-18");
        assert_eq!(negotiate_version(""), "2025-06-18");
    }
}

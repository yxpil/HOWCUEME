use crate::poll::{self, PollCtx};
use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Start the HTTP API server plus its internal polling loop.
pub async fn run_serve(host: &str, port: u16, ctx: Arc<PollCtx>) -> Result<()> {
    // Internal poller on a dedicated thread (blocking actions stay off the
    // async runtime; webhook/wake_bit use the blocking ureq client).
    let poller = ctx.clone();
    let interval = Duration::from_secs(ctx.poll_interval_secs.max(1));
    std::thread::spawn(move || loop {
        poll::poll_round(&poller, &mut |ev| {
            eprintln!(
                "[{}] rule '{}' triggered, ok={}",
                ev.triggered_at, ev.rule, ev.ok
            );
        });
        std::thread::sleep(interval);
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/rules", get(rules))
        .route("/invoke", post(invoke))
        .with_state(ctx);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let local = listener.local_addr()?;
    eprintln!("howcueme serve listening on http://{local}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn rules(State(ctx): State<Arc<PollCtx>>) -> Json<Value> {
    Json(json!({ "ok": true, "rules": poll::rules_snapshot(&ctx) }))
}

/// BIT Remote tool entry point. Accepts the BIT payload
/// `{"tool_id": "...", "tool": "...", "invoked_by": "...", "params": {...}}`
/// and routes on `params.action` (or `params.tool`):
/// `status | list | fire | validate`.
async fn invoke(
    State(ctx): State<Arc<PollCtx>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let params = body.get("params").cloned().unwrap_or_else(|| body.clone());
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .or_else(|| params.get("tool").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();

    match action.as_str() {
        "status" => Ok(Json(json!({ "ok": true, "status": status_of(&ctx) }))),
        "list" => Ok(Json(
            json!({ "ok": true, "rules": poll::rules_snapshot(&ctx) }),
        )),
        "fire" => {
            let name = params
                .get("rule")
                .and_then(Value::as_str)
                .or_else(|| params.get("name").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                return Err(bad_request("params.rule is required for action 'fire'"));
            }
            let res = tokio::task::spawn_blocking(move || poll::fire_rule(&ctx, &name)).await;
            match res {
                Ok(Ok(ev)) => Ok(Json(json!({ "ok": true, "event": ev.to_json() }))),
                Ok(Err(e)) => Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "ok": false, "error": e.to_string() })),
                )),
                Err(e) => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "ok": false, "error": e.to_string() })),
                )),
            }
        }
        "validate" => Ok(Json(
            json!({ "ok": true, "validate": validate_payload(&ctx) }),
        )),
        other => Err(bad_request(&format!(
            "unknown action '{other}' (expected status|list|fire|validate)"
        ))),
    }
}

fn bad_request(msg: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "ok": false, "error": msg })),
    )
}

fn status_of(ctx: &PollCtx) -> Value {
    let enabled = ctx.config.rules.iter().filter(|r| r.enabled).count();
    json!({
        "rules": ctx.config.rules.len(),
        "enabled_rules": enabled,
        "rules_file": ctx.rules_path.display().to_string(),
        "data_dir": ctx.state_path.parent().map(|p| p.display().to_string()),
        "poll_interval_secs": ctx.poll_interval_secs,
        "started_at_secs": ctx.started_at_secs,
        "uptime_secs": poll::now_secs().saturating_sub(ctx.started_at_secs),
    })
}

fn validate_payload(ctx: &PollCtx) -> Value {
    let errors = match std::fs::read_to_string(&ctx.rules_path) {
        Ok(text) => match crate::config::parse(&text) {
            Ok(cfg) => crate::config::validate(&cfg),
            Err(e) => vec![format!("{e:#}")],
        },
        Err(e) => vec![format!("cannot read {}: {e}", ctx.rules_path.display())],
    };
    json!({
        "valid": errors.is_empty(),
        "errors": errors,
        "rules_file": ctx.rules_path.display().to_string(),
    })
}

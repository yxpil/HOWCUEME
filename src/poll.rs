use crate::action;
use crate::cond;
use crate::config::{Action, Rule, When};
use crate::state::StateFile;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;

/// Result of evaluating one condition.
pub struct Outcome {
    pub fired: bool,
    pub result: Value,
    pub mtime_nanos: Option<i64>,
}

/// Shared context for the polling loop (used by both `run` and `serve`).
pub struct PollCtx {
    pub config: crate::config::Config,
    pub rules_path: PathBuf,
    pub state_path: PathBuf,
    pub state: Mutex<StateFile>,
    pub poll_interval_secs: u64,
    pub started_at_secs: i64,
}

/// One triggered-action event, emitted on stdout as a JSON line.
pub struct Event {
    pub rule: String,
    pub triggered_at: String,
    pub ok: bool,
    pub forced: bool,
    pub action: Value,
    pub when: Value,
    pub when_result: Value,
    pub action_result: Value,
}

impl Event {
    pub fn to_json(&self) -> Value {
        json!({
            "rule": self.rule,
            "triggered_at": self.triggered_at,
            "ok": self.ok,
            "forced": self.forced,
            "action": self.action,
            "when": self.when,
            "when_result": self.when_result,
            "result": self.action_result,
        })
    }
}

pub fn now_secs() -> i64 {
    Utc::now().timestamp()
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn in_cooldown(state: &StateFile, rule: &Rule, now_secs: i64) -> bool {
    if rule.cooldown_secs == 0 {
        return false;
    }
    state
        .rules
        .get(&rule.name)
        .and_then(|rs| rs.last_triggered_secs)
        .map(|t| now_secs.saturating_sub(t) < rule.cooldown_secs as i64)
        .unwrap_or(false)
}

/// Evaluate one rule condition against the given state snapshot.
pub fn evaluate(
    rule: &Rule,
    last_triggered_secs: Option<i64>,
    last_mtime_nanos: Option<i64>,
    now_secs: i64,
    now_local: DateTime<Local>,
) -> Outcome {
    match &rule.when {
        When::Interval { every_secs } => {
            let fired = cond::eval_interval(last_triggered_secs, *every_secs, now_secs);
            Outcome {
                fired,
                result: json!({
                    "every_secs": every_secs,
                    "last_triggered_secs": last_triggered_secs,
                    "secs_since_last": last_triggered_secs.map(|t| now_secs.saturating_sub(t)),
                }),
                mtime_nanos: None,
            }
        }
        When::Daily { at } => match chrono::NaiveTime::parse_from_str(at, "%H:%M") {
            Ok(t) => {
                let fired = cond::eval_daily(&t, now_local, last_triggered_secs);
                Outcome {
                    fired,
                    result: json!({
                        "at": at,
                        "local_now": now_local.format("%Y-%m-%d %H:%M:%S").to_string(),
                    }),
                    mtime_nanos: None,
                }
            }
            Err(e) => Outcome {
                fired: false,
                result: json!({ "error": format!("invalid daily.at '{at}': {e}") }),
                mtime_nanos: None,
            },
        },
        When::File { path, op } => {
            let (fired, mtime) = cond::eval_file(path, *op, last_mtime_nanos);
            Outcome {
                fired,
                result: json!({ "path": path, "op": op, "exists": mtime.is_some(), "mtime_nanos": mtime }),
                mtime_nanos: mtime,
            }
        }
        When::Http {
            url,
            expect_status,
            timeout_secs,
        } => {
            let (fired, result) = cond::eval_http(url, *expect_status, *timeout_secs);
            Outcome {
                fired,
                result,
                mtime_nanos: None,
            }
        }
        When::Process { name, op } => {
            let (fired, result) = cond::eval_process(name, *op);
            Outcome {
                fired,
                result,
                mtime_nanos: None,
            }
        }
    }
}

/// Run one polling round over all enabled rules. Returns the number of
/// rules that fired. `emit` is called once per triggered action.
pub fn poll_round(ctx: &PollCtx, emit: &mut dyn FnMut(&Event)) -> usize {
    let now_secs = now_secs();
    let now_local = Local::now();
    let mut fired_count = 0;

    for rule in &ctx.config.rules {
        if !rule.enabled {
            continue;
        }
        let snapshot = {
            let state = ctx.state.lock().unwrap();
            if in_cooldown(&state, rule, now_secs) {
                continue;
            }
            state.rules.get(&rule.name).cloned().unwrap_or_default()
        };

        let outcome = evaluate(
            rule,
            snapshot.last_triggered_secs,
            snapshot.last_mtime_nanos,
            now_secs,
            now_local,
        );

        let mut dirty = false;
        if outcome.mtime_nanos != snapshot.last_mtime_nanos {
            let mut state = ctx.state.lock().unwrap();
            state
                .rules
                .entry(rule.name.clone())
                .or_default()
                .last_mtime_nanos = outcome.mtime_nanos;
            dirty = true;
        }

        if outcome.fired {
            let when_cfg = serde_json::to_value(&rule.when).unwrap_or(Value::Null);
            let triggered_at = now_rfc3339();
            let executed = action::execute(
                &rule.action,
                &rule.name,
                &triggered_at,
                &when_cfg,
                &outcome.result,
            );
            {
                let mut state = ctx.state.lock().unwrap();
                state
                    .rules
                    .entry(rule.name.clone())
                    .or_default()
                    .last_triggered_secs = Some(now_secs);
            }
            let event = Event {
                rule: rule.name.clone(),
                triggered_at,
                ok: executed.ok,
                forced: false,
                action: serde_json::to_value(&rule.action).unwrap_or(Value::Null),
                when: when_cfg,
                when_result: outcome.result,
                action_result: executed.result,
            };
            emit(&event);
            fired_count += 1;
            dirty = true;
        }

        if dirty {
            let _ = save_state(ctx);
        }
    }
    fired_count
}

/// Force-trigger a rule now, bypassing condition and cooldown.
/// Still records `last_triggered` in state.
pub fn fire_rule(ctx: &PollCtx, name: &str) -> Result<Event> {
    let rule = ctx
        .config
        .rules
        .iter()
        .find(|r| r.name == name)
        .cloned()
        .ok_or_else(|| anyhow!("rule not found: {name}"))?;

    let now_secs = now_secs();
    let triggered_at = now_rfc3339();
    let when_cfg = serde_json::to_value(&rule.when).unwrap_or(Value::Null);
    let when_result = json!({ "forced": true });
    let executed = action::execute(
        &rule.action,
        &rule.name,
        &triggered_at,
        &when_cfg,
        &when_result,
    );
    {
        let mut state = ctx.state.lock().unwrap();
        state
            .rules
            .entry(rule.name.clone())
            .or_default()
            .last_triggered_secs = Some(now_secs);
    }
    save_state(ctx)?;

    Ok(Event {
        rule: rule.name,
        triggered_at,
        ok: executed.ok,
        forced: true,
        action: serde_json::to_value(&rule.action).unwrap_or(Value::Null),
        when: when_cfg,
        when_result,
        action_result: executed.result,
    })
}

/// Persist the current state snapshot to disk.
pub fn save_state(ctx: &PollCtx) -> Result<()> {
    let snapshot = ctx.state.lock().unwrap().clone();
    snapshot.save(&ctx.state_path)
}

/// Snapshot of all rules plus their persisted state (used by `list` and API).
pub fn rules_snapshot(ctx: &PollCtx) -> Vec<Value> {
    let state = ctx.state.lock().unwrap().clone();
    ctx.config
        .rules
        .iter()
        .map(|r| {
            let rs = state.rules.get(&r.name);
            let last = rs.and_then(|s| s.last_triggered_secs);
            let last_iso = last
                .and_then(|s| DateTime::from_timestamp(s, 0))
                .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true));
            json!({
                "name": r.name,
                "enabled": r.enabled,
                "cooldown_secs": r.cooldown_secs,
                "when": r.when,
                "action": r.action,
                "last_triggered_secs": last,
                "last_triggered": last_iso,
            })
        })
        .collect()
}

/// Short human-readable description of a condition.
pub fn when_desc(w: &When) -> String {
    match w {
        When::Interval { every_secs } => format!("interval every {every_secs}s"),
        When::Daily { at } => format!("daily at {at}"),
        When::File { path, op } => format!("file {op} {path}"),
        When::Http {
            url, expect_status, ..
        } => format!("http expect {expect_status} {url}"),
        When::Process { name, op } => format!("process {op} {name}"),
    }
}

/// Short human-readable description of an action.
pub fn action_desc(a: &Action) -> String {
    match a {
        Action::Webhook { url } => format!("webhook {url}"),
        Action::Command { cmd, args } => format!("command {cmd} {}", args.join(" ")),
        Action::WakeBit { bit_url, .. } => format!("wake_bit {bit_url}/api/chat"),
    }
}

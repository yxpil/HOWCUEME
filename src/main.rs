mod action;
mod cond;
mod config;
mod poll;
mod serve;
mod state;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Map, Value};
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Conditional self-wakeup daemon for AI agents (BIT ecosystem).
#[derive(Parser)]
#[command(
    name = "howcueme",
    version,
    about = "Conditional self-wakeup daemon for AI agents (BIT ecosystem)"
)]
struct Cli {
    /// Path to the rules TOML file (default: <data_dir>/rules.toml)
    #[arg(short = 'c', long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Emit structured JSON output where applicable
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate the rules file and report errors
    Validate,
    /// Run the polling daemon (or a single round with --once)
    Run {
        /// Evaluate a single round and exit (no network side effects unless a rule fires)
        #[arg(long)]
        once: bool,
        /// Seconds between polling rounds
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },
    /// List rules with their current persisted state
    List,
    /// Force-trigger a rule's action now (bypasses condition and cooldown)
    Fire {
        /// Rule name to trigger (or pipe {"rule": "..."} via stdin)
        #[arg(value_name = "RULE_NAME")]
        rule: Option<String>,
    },
    /// Run the HTTP API server (default 127.0.0.1:8752) with an internal poller
    Serve {
        /// TCP port to bind
        #[arg(long, default_value_t = 8752)]
        port: u16,
        /// Host/interface to bind
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let overrides = stdin_overrides();
    match cli.command {
        Command::Validate => validate_cmd(&cli, overrides.as_ref()),
        Command::Run { once, interval } => run_cmd(&cli, once, interval, overrides.as_ref()),
        Command::List => list_cmd(&cli, overrides.as_ref()),
        Command::Fire { ref rule } => fire_cmd(&cli, rule.as_deref(), overrides.as_ref()),
        Command::Serve { port, ref host } => serve_cmd(&cli, port, host, overrides.as_ref()),
    }
}

/// When stdin is piped (not a TTY), read a JSON object and merge it over CLI
/// args (stdin wins) — the BIT exec-mode contract. Waits at most ~300ms for
/// piped stdin so long-lived daemons never block on an open pipe.
fn stdin_overrides() -> Option<Map<String, Value>> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    const STDIN_WAIT: Duration = Duration::from_millis(300);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let ok = std::io::stdin().read_to_string(&mut buf).is_ok();
        let _ = tx.send((ok, buf));
    });
    let (ok, buf) = rx.recv_timeout(STDIN_WAIT).ok()?;
    if !ok {
        return None;
    }
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return None;
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(map)) => Some(map),
        Ok(_) => {
            eprintln!("warning: piped stdin JSON is not an object, ignored");
            None
        }
        Err(e) => {
            eprintln!("warning: failed to parse piped stdin JSON ({e}), ignored");
            None
        }
    }
}

fn ov_str(ov: Option<&Map<String, Value>>, keys: &[&str]) -> Option<String> {
    let ov = ov?;
    for k in keys {
        if let Some(s) = ov.get(*k).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

fn ov_bool(ov: Option<&Map<String, Value>>, key: &str) -> Option<bool> {
    ov.and_then(|m| m.get(key)).and_then(Value::as_bool)
}

fn ov_u64(ov: Option<&Map<String, Value>>, key: &str) -> Option<u64> {
    ov.and_then(|m| m.get(key)).and_then(Value::as_u64)
}

fn resolve_rules_path(flag: Option<&PathBuf>, ov: Option<&Map<String, Value>>) -> Result<PathBuf> {
    let data = config::data_dir()?;
    Ok(match flag {
        Some(p) => p.clone(),
        None => match ov_str(ov, &["config", "rules_file"]) {
            Some(s) => PathBuf::from(s),
            None => data.join("rules.toml"),
        },
    })
}

fn load_ctx(
    config_flag: Option<&PathBuf>,
    ov: Option<&Map<String, Value>>,
    poll_interval_secs: u64,
) -> Result<Arc<poll::PollCtx>> {
    let data = config::data_dir()?;
    let rules_path = resolve_rules_path(config_flag, ov)?;
    let text = std::fs::read_to_string(&rules_path)
        .with_context(|| format!("cannot read rules file {}", rules_path.display()))?;
    let cfg = config::parse(&text)
        .with_context(|| format!("invalid rules file {}", rules_path.display()))?;
    let state_path = config::state_path(&data);
    let st = state::StateFile::load(&state_path);
    Ok(Arc::new(poll::PollCtx {
        config: cfg,
        rules_path,
        state_path,
        state: Mutex::new(st),
        poll_interval_secs,
        started_at_secs: poll::now_secs(),
    }))
}

fn emit_event(ev: &poll::Event) {
    eprintln!(
        "[{}] rule '{}' triggered, ok={}",
        ev.triggered_at, ev.rule, ev.ok
    );
    println!("{}", ev.to_json());
}

fn validate_cmd(cli: &Cli, ov: Option<&Map<String, Value>>) -> Result<ExitCode> {
    let path = resolve_rules_path(cli.config.as_ref(), ov)?;
    let (cfg, errors) = match std::fs::read_to_string(&path) {
        Ok(text) => match config::parse(&text) {
            Ok(c) => {
                let errs = config::validate(&c);
                (Some(c), errs)
            }
            Err(e) => (None, vec![format!("{e:#}")]),
        },
        Err(e) => (None, vec![format!("cannot read {}: {e}", path.display())]),
    };
    let valid = errors.is_empty();
    if cli.json {
        let names: Vec<String> = cfg
            .as_ref()
            .map(|c| c.rules.iter().map(|r| r.name.clone()).collect())
            .unwrap_or_default();
        println!(
            "{}",
            json!({
                "ok": valid,
                "path": path.display().to_string(),
                "rules_count": names.len(),
                "rules": names,
                "errors": errors,
            })
        );
    } else {
        println!("rules file: {}", path.display());
        if valid {
            println!(
                "OK: {} rule(s), no errors",
                cfg.map(|c| c.rules.len()).unwrap_or(0)
            );
        } else {
            for e in &errors {
                println!("ERROR: {e}");
            }
        }
    }
    Ok(if valid {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn run_cmd(
    cli: &Cli,
    once_flag: bool,
    interval_arg: u64,
    ov: Option<&Map<String, Value>>,
) -> Result<ExitCode> {
    let once = once_flag || ov_bool(ov, "once").unwrap_or(false);
    let interval = ov_u64(ov, "interval").unwrap_or(interval_arg).max(1);
    let ctx = load_ctx(cli.config.as_ref(), ov, interval)?;
    if once {
        poll::poll_round(&ctx, &mut emit_event);
    } else {
        eprintln!(
            "howcueme run: {} rule(s), polling every {interval}s (Ctrl-C to stop)",
            ctx.config.rules.len()
        );
        loop {
            poll::poll_round(&ctx, &mut emit_event);
            std::thread::sleep(Duration::from_secs(interval));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn list_cmd(cli: &Cli, ov: Option<&Map<String, Value>>) -> Result<ExitCode> {
    let ctx = load_ctx(cli.config.as_ref(), ov, 5)?;
    if cli.json {
        println!(
            "{}",
            json!({ "ok": true, "rules": poll::rules_snapshot(&ctx) })
        );
        return Ok(ExitCode::SUCCESS);
    }
    if ctx.config.rules.is_empty() {
        println!("no rules configured ({})", ctx.rules_path.display());
        return Ok(ExitCode::SUCCESS);
    }
    let st = ctx.state.lock().unwrap();
    println!(
        "{:<26} {:<9} {:<34} {:<34} LAST TRIGGERED",
        "NAME", "ENABLED", "WHEN", "ACTION"
    );
    for r in &ctx.config.rules {
        let last = st.rules.get(&r.name).and_then(|s| s.last_triggered_secs);
        let last_disp = match last {
            Some(s) => chrono::DateTime::from_timestamp(s, 0)
                .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                .unwrap_or_else(|| s.to_string()),
            None => "-".to_string(),
        };
        println!(
            "{:<26} {:<9} {:<34} {:<34} {}",
            r.name,
            if r.enabled { "yes" } else { "no" },
            poll::when_desc(&r.when),
            poll::action_desc(&r.action),
            last_disp
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn fire_cmd(
    cli: &Cli,
    rule_arg: Option<&str>,
    ov: Option<&Map<String, Value>>,
) -> Result<ExitCode> {
    let ctx = load_ctx(cli.config.as_ref(), ov, 5)?;
    let name = match rule_arg {
        Some(n) if !n.trim().is_empty() => n.to_string(),
        _ => ov_str(ov, &["rule", "name"]).context(
            "missing rule name: pass positional RULE_NAME or pipe {\"rule\": \"...\"} via stdin",
        )?,
    };
    match poll::fire_rule(&ctx, &name) {
        Ok(ev) => {
            println!("{}", ev.to_json());
            eprintln!(
                "[{}] fired rule '{}', ok={}",
                ev.triggered_at, ev.rule, ev.ok
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            Ok(ExitCode::from(1))
        }
    }
}

fn serve_cmd(
    cli: &Cli,
    port_arg: u16,
    host_arg: &str,
    ov: Option<&Map<String, Value>>,
) -> Result<ExitCode> {
    let ctx = load_ctx(cli.config.as_ref(), ov, 5)?;
    let port = ov_u64(ov, "port")
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(port_arg);
    let host = ov_str(ov, &["host"]).unwrap_or_else(|| host_arg.to_string());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start async runtime")?;
    runtime.block_on(serve::run_serve(&host, port, ctx))?;
    Ok(ExitCode::SUCCESS)
}

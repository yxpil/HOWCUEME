//! Integration tests running the real `howcueme` binary.

use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_howcueme")
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("howcueme-cli-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_bin(args: &[&str], data_dir: &Path, stdin_text: Option<&str>) -> Output {
    let mut cmd = Command::new(bin_path());
    cmd.args(args).env("HOWCUEME_DATA_DIR", data_dir);
    match stdin_text {
        Some(s) => {
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = cmd.spawn().unwrap();
            child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
            child.wait_with_output().unwrap()
        }
        None => {
            cmd.stdin(Stdio::null());
            cmd.output().unwrap()
        }
    }
}

fn stdout_json_lines(out: &Output) -> Vec<Value> {
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("stdout must be JSON lines"))
        .collect()
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

#[test]
fn validate_reports_ok_and_errors() {
    let dir = unique_temp_dir("validate");
    let rules = dir.join("rules.toml");
    let trigger = dir.join("trigger.txt");
    let done = dir.join("done.txt");
    fs::write(
        &rules,
        format!(
            "[[rule]]\nname = \"validate-me\"\n[rule.when]\ntype = \"file\"\npath = '{}'\nop = \"exists\"\n[rule.action]\n{}\n",
            trigger.display(),
            command_action(&done)
        ),
    )
    .unwrap();

    // --config supplied via piped stdin JSON (BIT exec-mode contract)
    let out = run_bin(
        &["validate", "--json"],
        &dir,
        Some(&format!("{{\"config\": \"{}\"}}", rules.display())),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["rules_count"], 1);
    assert_eq!(v["rules"][0], "validate-me");

    // broken TOML -> exit 1 with errors
    let bad = dir.join("bad.toml");
    fs::write(&bad, "<<< not toml >>>").unwrap();
    let out = run_bin(
        &["validate", "--json", "-c", bad.to_str().unwrap()],
        &dir,
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert!(!v["errors"].as_array().unwrap().is_empty());

    // semantic error: duplicate names
    let dup = dir.join("dup.toml");
    fs::write(
        &dup,
        "[[rule]]\nname = \"dup\"\n[rule.when]\ntype = \"interval\"\nevery_secs = 5\n[rule.action]\ntype = \"command\"\ncmd = \"echo\"\n\n[[rule]]\nname = \"dup\"\n[rule.when]\ntype = \"interval\"\nevery_secs = 5\n[rule.action]\ntype = \"command\"\ncmd = \"echo\"\n",
    )
    .unwrap();
    let out = run_bin(
        &["validate", "--json", "-c", dup.to_str().unwrap()],
        &dir,
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(serde_json::to_string(&v["errors"])
        .unwrap()
        .contains("duplicate"));

    // missing file -> exit 1
    let out = run_bin(
        &["validate", "-c", "/definitely/missing/rules.toml"],
        &dir,
        None,
    );
    assert_eq!(out.status.code(), Some(1));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn run_once_triggers_command_action_and_respects_cooldown() {
    let dir = unique_temp_dir("once");
    let rules = dir.join("rules.toml");
    let trigger = dir.join("trigger.txt");
    let done = dir.join("done.txt");
    fs::write(&trigger, b"go").unwrap();
    fs::write(
        &rules,
        format!(
            "[[rule]]\nname = \"once-cmd\"\ncooldown_secs = 600\n[rule.when]\ntype = \"file\"\npath = '{}'\nop = \"exists\"\n[rule.action]\n{}\n",
            trigger.display(),
            command_action(&done)
        ),
    )
    .unwrap();

    // first --once round: file exists -> fires -> command runs
    let out = run_bin(
        &["run", "--once", "-c", rules.to_str().unwrap()],
        &dir,
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = stdout_json_lines(&out);
    assert_eq!(
        events.len(),
        1,
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(events[0]["rule"], "once-cmd");
    assert_eq!(events[0]["ok"], true);
    assert_eq!(events[0]["forced"], false);
    assert_eq!(events[0]["when"]["type"], "file");
    assert_eq!(events[0]["when_result"]["exists"], true);
    assert_eq!(events[0]["result"]["exit_code"], 0);
    assert!(done.exists(), "command action side effect missing");

    // state persisted under HOWCUEME_DATA_DIR
    let state_path = dir.join("state.json");
    let state: Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let first_trigger = state["rules"]["once-cmd"]["last_triggered_secs"]
        .as_i64()
        .expect("last_triggered_secs recorded");
    assert!(first_trigger > 0);

    // second --once round: cooldown blocks -> no events, state unchanged
    let out = run_bin(
        &["run", "--once", "-c", rules.to_str().unwrap()],
        &dir,
        None,
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "expected no events during cooldown");
    let state: Value = serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(
        state["rules"]["once-cmd"]["last_triggered_secs"].as_i64(),
        Some(first_trigger)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fire_cli_forces_trigger_and_accepts_stdin_rule() {
    let dir = unique_temp_dir("fire");
    let rules = dir.join("rules.toml");
    let done = dir.join("done.txt");
    fs::write(
        &rules,
        format!(
            "[[rule]]\nname = \"fire-cmd\"\n[rule.when]\ntype = \"interval\"\nevery_secs = 3600\n[rule.action]\n{}\n",
            command_action(&done)
        ),
    )
    .unwrap();

    // positional rule name
    let out = run_bin(
        &["fire", "fire-cmd", "-c", rules.to_str().unwrap()],
        &dir,
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = stdout_json_lines(&out);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["rule"], "fire-cmd");
    assert_eq!(events[0]["forced"], true);
    assert_eq!(events[0]["result"]["exit_code"], 0);

    // unknown rule -> exit 1
    let out = run_bin(
        &["fire", "missing-rule", "-c", rules.to_str().unwrap()],
        &dir,
        None,
    );
    assert_eq!(out.status.code(), Some(1));

    // piped stdin JSON overrides / supplies the rule name
    let out = run_bin(
        &["fire", "-c", rules.to_str().unwrap()],
        &dir,
        Some("{\"rule\": \"fire-cmd\"}"),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = stdout_json_lines(&out);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["rule"], "fire-cmd");

    // list shows updated last_triggered
    let out = run_bin(
        &["list", "--json", "-c", rules.to_str().unwrap()],
        &dir,
        None,
    );
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["rules"][0]["name"], "fire-cmd");
    assert!(v["rules"][0]["last_triggered_secs"].as_i64().unwrap() > 0);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn serve_exposes_health_rules_and_invoke() {
    use std::io::BufRead;
    use std::io::BufReader;

    let dir = unique_temp_dir("serve");
    let rules = dir.join("rules.toml");
    // condition never fires so the internal poller stays quiet and deterministic
    fs::write(
        &rules,
        format!(
            "[[rule]]\nname = \"serve-fire\"\n[rule.when]\ntype = \"process\"\nname = \"howcueme-no-such-proc-xyz\"\nop = \"exists\"\n[rule.action]\n{}\n",
            command_action(&dir.join("serve-done.txt"))
        ),
    )
    .unwrap();

    let mut child = Command::new(bin_path())
        .args(["serve", "--port", "0"])
        .env("HOWCUEME_DATA_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<u16>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.contains("listening on http://") {
                if let Some(port) = line
                    .rsplit(':')
                    .next()
                    .and_then(|p| p.trim().parse::<u16>().ok())
                {
                    let _ = tx.send(port);
                }
            }
        }
    });

    let port = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("serve must print its listening address");
    let base = format!("http://127.0.0.1:{port}");

    // GET /health (with retries until the server accepts connections)
    let mut health_ok = false;
    for _ in 0..50 {
        match ureq::get(&format!("{base}/health"))
            .timeout(Duration::from_secs(2))
            .call()
        {
            Ok(resp) => {
                assert_eq!(resp.status(), 200);
                let v: Value = resp.into_json().unwrap();
                assert_eq!(v["ok"], true);
                health_ok = true;
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    assert!(health_ok, "server never became healthy");

    // GET /rules
    let resp = ureq::get(&format!("{base}/rules"))
        .timeout(Duration::from_secs(3))
        .call()
        .unwrap();
    let v: Value = resp.into_json().unwrap();
    assert_eq!(v["rules"][0]["name"], "serve-fire");

    // POST /invoke status
    let resp = ureq::post(&format!("{base}/invoke"))
        .timeout(Duration::from_secs(3))
        .send_json(serde_json::json!({
            "tool_id": "t1", "tool": "howcueme", "invoked_by": "integration-test",
            "params": { "action": "status" }
        }))
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.into_json().unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["status"]["rules"], 1);
    assert_eq!(v["status"]["enabled_rules"], 1);

    // POST /invoke fire -> executes the rule action
    let resp = ureq::post(&format!("{base}/invoke"))
        .timeout(Duration::from_secs(15))
        .send_json(serde_json::json!({
            "tool_id": "t1", "tool": "howcueme", "invoked_by": "integration-test",
            "params": { "action": "fire", "rule": "serve-fire" }
        }))
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.into_json().unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["event"]["rule"], "serve-fire");
    assert_eq!(v["event"]["forced"], true);
    assert_eq!(v["event"]["result"]["exit_code"], 0);

    // POST /invoke validate
    let resp = ureq::post(&format!("{base}/invoke"))
        .timeout(Duration::from_secs(3))
        .send_json(serde_json::json!({ "params": { "action": "validate" } }))
        .unwrap();
    let v: Value = resp.into_json().unwrap();
    assert_eq!(v["validate"]["valid"], true);

    // POST /invoke fire unknown rule -> 404
    let err = ureq::post(&format!("{base}/invoke"))
        .timeout(Duration::from_secs(3))
        .send_json(serde_json::json!({ "params": { "action": "fire", "rule": "nope" } }))
        .unwrap_err();
    match err {
        ureq::Error::Status(code, _) => assert_eq!(code, 404),
        other => panic!("expected 404, got {other}"),
    }

    // POST /invoke unknown action -> 400
    let err = ureq::post(&format!("{base}/invoke"))
        .timeout(Duration::from_secs(3))
        .send_json(serde_json::json!({ "params": { "action": "bogus" } }))
        .unwrap_err();
    match err {
        ureq::Error::Status(code, _) => assert_eq!(code, 400),
        other => panic!("expected 400, got {other}"),
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    let _ = fs::remove_dir_all(&dir);
}

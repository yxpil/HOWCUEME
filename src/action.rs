use crate::config::Action;
use serde_json::{json, Value};
use std::time::Duration;

/// Outcome of executing one action.
pub struct Executed {
    pub ok: bool,
    pub result: Value,
}

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Execute an action for a fired rule. Never panics; failures are reported
/// in the returned result value (`ok: false` + `error` field).
pub fn execute(
    action: &Action,
    rule: &str,
    triggered_at: &str,
    when: &Value,
    when_result: &Value,
) -> Executed {
    match action {
        Action::Webhook { url } => webhook(url, rule, triggered_at, when, when_result),
        Action::Command { cmd, args } => command(cmd, args),
        Action::WakeBit {
            bit_url,
            client_key,
            prompt,
        } => wake_bit(bit_url, client_key, prompt),
    }
}

/// POST the trigger payload as JSON to `url`.
fn webhook(
    url: &str,
    rule: &str,
    triggered_at: &str,
    when: &Value,
    when_result: &Value,
) -> Executed {
    let body = json!({
        "rule": rule,
        "triggered_at": triggered_at,
        "when": when,
        "result": when_result,
    });
    match ureq::post(url).timeout(HTTP_TIMEOUT).send_json(&body) {
        Ok(resp) => {
            let status = resp.status();
            Executed {
                ok: (200..300).contains(&status),
                result: json!({ "status": status }),
            }
        }
        Err(ureq::Error::Status(status, _)) => Executed {
            ok: false,
            result: json!({ "status": status }),
        },
        Err(e) => Executed {
            ok: false,
            result: json!({ "error": e.to_string() }),
        },
    }
}

/// Run an external command directly (no shell involved).
fn command(cmd: &str, args: &[String]) -> Executed {
    match std::process::Command::new(cmd).args(args).output() {
        Ok(out) => {
            let code = out.status.code().unwrap_or(-1);
            Executed {
                ok: out.status.success(),
                result: json!({
                    "exit_code": code,
                    "stdout": String::from_utf8_lossy(&out.stdout),
                    "stderr": String::from_utf8_lossy(&out.stderr),
                }),
            }
        }
        Err(e) => Executed {
            ok: false,
            result: json!({ "error": e.to_string() }),
        },
    }
}

/// Wake a BIT agent: POST `{bit_url}/api/chat` with
/// `Authorization: Bearer <client_key>` and body `{"message": prompt}`.
fn wake_bit(bit_url: &str, client_key: &str, prompt: &str) -> Executed {
    let url = format!("{}/api/chat", bit_url.trim_end_matches('/'));
    let body = json!({ "message": prompt });
    match ureq::post(&url)
        .timeout(HTTP_TIMEOUT)
        .set("Authorization", &format!("Bearer {client_key}"))
        .send_json(&body)
    {
        Ok(resp) => {
            let status = resp.status();
            let reply = resp.into_string().unwrap_or_default();
            Executed {
                ok: (200..300).contains(&status),
                result: json!({ "status": status, "reply": reply }),
            }
        }
        Err(ureq::Error::Status(status, resp)) => Executed {
            ok: false,
            result: json!({ "status": status, "reply": resp.into_string().unwrap_or_default() }),
        },
        Err(e) => Executed {
            ok: false,
            result: json!({ "error": e.to_string() }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::Receiver;

    /// One-shot server that captures the raw HTTP request text.
    fn spawn_capture_server() -> (String, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match Read::read(&mut stream, &mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            data.extend_from_slice(&buf[..n]);
                            let s = String::from_utf8_lossy(&data).to_string();
                            if let Some(pos) = s.find("\r\n\r\n") {
                                let cl: usize = s
                                    .lines()
                                    .find_map(|l| {
                                        l.to_ascii_lowercase()
                                            .strip_prefix("content-length:")
                                            .map(|v| v.trim().parse().unwrap_or(0))
                                    })
                                    .unwrap_or(0);
                                if data.len() >= pos + 4 + cl {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = Write::write_all(
                    &mut stream,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
                let _ = tx.send(String::from_utf8_lossy(&data).to_string());
            }
        });
        (format!("http://{addr}"), rx)
    }

    #[test]
    fn command_action_runs_and_captures_output() {
        #[cfg(unix)]
        let (cmd, args) = ("echo".to_string(), vec!["hello-action".to_string()]);
        #[cfg(windows)]
        let (cmd, args) = (
            "cmd".to_string(),
            vec!["/C".to_string(), "echo hello-action".to_string()],
        );

        let ex = execute(
            &Action::Command { cmd, args },
            "cmd-rule",
            "2026-09-04T00:00:00Z",
            &json!({}),
            &json!({}),
        );
        assert!(ex.ok);
        assert_eq!(ex.result["exit_code"], 0);
        assert!(ex.result["stdout"]
            .as_str()
            .unwrap()
            .contains("hello-action"));
    }

    #[test]
    fn command_action_reports_spawn_failure() {
        let ex = execute(
            &Action::Command {
                cmd: "howcueme-definitely-missing-binary-9f3a1".to_string(),
                args: vec![],
            },
            "cmd-rule",
            "2026-09-04T00:00:00Z",
            &json!({}),
            &json!({}),
        );
        assert!(!ex.ok);
        assert!(ex.result["error"].as_str().is_some());
    }

    #[test]
    fn webhook_posts_rule_payload() {
        let (url, rx) = spawn_capture_server();
        let ex = execute(
            &Action::Webhook { url: url.clone() },
            "hook-rule",
            "2026-09-04T08:00:00Z",
            &json!({ "type": "interval", "every_secs": 60 }),
            &json!({ "secs_since_last": 60 }),
        );
        assert!(ex.ok);
        assert_eq!(ex.result["status"], 200);

        let req = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(req.starts_with("POST / HTTP/1.1"), "request line: {req}");
        let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
        let v: Value = serde_json::from_str(body).expect("webhook body must be JSON");
        assert_eq!(v["rule"], "hook-rule");
        assert_eq!(v["triggered_at"], "2026-09-04T08:00:00Z");
        assert_eq!(v["when"]["type"], "interval");
        assert_eq!(v["result"]["secs_since_last"], 60);
    }

    #[test]
    fn wake_bit_posts_chat_with_bearer_auth() {
        let (url, rx) = spawn_capture_server();
        let ex = execute(
            &Action::WakeBit {
                bit_url: url.clone(),
                client_key: "sk-test-123".to_string(),
                prompt: "wake up now".to_string(),
            },
            "wake-rule",
            "2026-09-04T08:00:00Z",
            &json!({}),
            &json!({ "forced": true }),
        );
        assert!(ex.ok);
        assert_eq!(ex.result["status"], 200);

        let req = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let lowered = req.to_ascii_lowercase();
        assert!(
            lowered.starts_with("post /api/chat http/1.1"),
            "request line: {req}"
        );
        assert!(
            lowered.contains("authorization: bearer sk-test-123"),
            "auth header missing: {req}"
        );
        let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
        let v: Value = serde_json::from_str(body).expect("wake_bit body must be JSON");
        assert_eq!(v["message"], "wake up now");
    }

    #[test]
    fn webhook_transport_error_is_reported() {
        let ex = execute(
            &Action::Webhook {
                url: "http://127.0.0.1:9/hook".to_string(),
            },
            "hook-rule",
            "2026-09-04T08:00:00Z",
            &json!({}),
            &json!({}),
        );
        assert!(!ex.ok);
        assert!(ex.result.get("error").is_some() || ex.result.get("status").is_some());
    }
}

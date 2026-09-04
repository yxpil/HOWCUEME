use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Root of the rules TOML file: `[[rule]]` array of tables.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "rule", default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub cooldown_secs: u64,
    pub when: When,
    pub action: Action,
}

fn default_true() -> bool {
    true
}

/// Trigger condition, exactly one per rule (internally tagged on `type`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum When {
    Interval {
        every_secs: u64,
    },
    Daily {
        at: String,
    },
    File {
        path: String,
        op: FileOp,
    },
    Http {
        url: String,
        #[serde(default = "default_expect_status")]
        expect_status: u16,
        #[serde(default = "default_timeout_secs")]
        timeout_secs: u64,
    },
    Process {
        name: String,
        op: ProcOp,
    },
}

fn default_expect_status() -> u16 {
    200
}

fn default_timeout_secs() -> u64 {
    5
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileOp {
    Exists,
    Changed,
}

impl std::fmt::Display for FileOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileOp::Exists => write!(f, "exists"),
            FileOp::Changed => write!(f, "changed"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProcOp {
    Exists,
    Absent,
}

impl std::fmt::Display for ProcOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcOp::Exists => write!(f, "exists"),
            ProcOp::Absent => write!(f, "absent"),
        }
    }
}

/// Action executed when a condition fires (internally tagged on `type`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Webhook {
        url: String,
    },
    Command {
        cmd: String,
        #[serde(default)]
        args: Vec<String>,
    },
    WakeBit {
        bit_url: String,
        client_key: String,
        prompt: String,
    },
}

/// Parse rules TOML text. Tolerates `cooldown_secs` / `enabled` written
/// after `[rule.when]` / `[rule.action]` table headers by lifting them
/// back to the rule level before deserialization.
pub fn parse(text: &str) -> Result<Config> {
    let mut root: toml::Value = toml::from_str(text).context("invalid TOML")?;
    if let Some(rules) = root.get_mut("rule").and_then(|v| v.as_array_mut()) {
        for r in rules.iter_mut() {
            if let Some(table) = r.as_table_mut() {
                lift_flag(table, "cooldown_secs");
                lift_flag(table, "enabled");
            }
        }
    }
    from_value(root).context("invalid rule structure")
}

fn lift_flag(rule_table: &mut toml::map::Map<String, toml::Value>, key: &str) {
    if rule_table.contains_key(key) {
        return;
    }
    for sub in ["when", "action"] {
        if let Some(t) = rule_table.get_mut(sub).and_then(|v| v.as_table_mut()) {
            if let Some(v) = t.remove(key) {
                rule_table.insert(key.to_string(), v);
                return;
            }
        }
    }
}

fn from_value<T: DeserializeOwned>(v: toml::Value) -> Result<T> {
    T::deserialize(v).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Semantic validation beyond TOML structure. Returns human-readable errors.
pub fn validate(cfg: &Config) -> Vec<String> {
    let mut errors = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (i, rule) in cfg.rules.iter().enumerate() {
        let tag = format!("rule[{i}] '{}'", rule.name);
        if rule.name.trim().is_empty() {
            errors.push(format!("rule[{i}]: name must not be empty"));
        } else if !seen.insert(rule.name.clone()) {
            errors.push(format!("{tag}: duplicate rule name"));
        }
        match &rule.when {
            When::Interval { every_secs } => {
                if *every_secs == 0 {
                    errors.push(format!("{tag}: when.interval.every_secs must be > 0"));
                }
            }
            When::Daily { at } => {
                if chrono::NaiveTime::parse_from_str(at, "%H:%M").is_err() {
                    errors.push(format!(
                        "{tag}: when.daily.at '{at}' must be HH:MM (00:00-23:59)"
                    ));
                }
            }
            When::File { path, .. } => {
                if path.trim().is_empty() {
                    errors.push(format!("{tag}: when.file.path must not be empty"));
                }
            }
            When::Http { url, .. } => {
                if !is_http_url(url) {
                    errors.push(format!(
                        "{tag}: when.http.url must start with http:// or https://"
                    ));
                }
            }
            When::Process { name, .. } => {
                if name.trim().is_empty() {
                    errors.push(format!("{tag}: when.process.name must not be empty"));
                }
            }
        }
        match &rule.action {
            Action::Webhook { url } => {
                if !is_http_url(url) {
                    errors.push(format!(
                        "{tag}: action.webhook.url must start with http:// or https://"
                    ));
                }
            }
            Action::Command { cmd, .. } => {
                if cmd.trim().is_empty() {
                    errors.push(format!("{tag}: action.command.cmd must not be empty"));
                }
            }
            Action::WakeBit {
                bit_url,
                client_key,
                ..
            } => {
                if !is_http_url(bit_url) {
                    errors.push(format!(
                        "{tag}: action.wake_bit.bit_url must start with http:// or https://"
                    ));
                }
                if client_key.trim().is_empty() {
                    errors.push(format!(
                        "{tag}: action.wake_bit.client_key must not be empty"
                    ));
                }
            }
        }
    }
    errors
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Data directory: `$HOWCUEME_DATA_DIR` if set, else `~/.howcueme`.
pub fn data_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("HOWCUEME_DATA_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let home = dirs::home_dir()
        .context("cannot determine the home directory (set HOWCUEME_DATA_DIR to override)")?;
    Ok(home.join(".howcueme"))
}

pub fn state_path(data: &Path) -> PathBuf {
    data.join("state.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[[rule]]
name = "wake-bit-daily"
[rule.when]
type = "interval"
every_secs = 60
[rule.action]
type = "webhook"
url = "http://127.0.0.1:8752/hook"
cooldown_secs = 300
enabled = true

[[rule]]
name = "daily-report"
[rule.when]
type = "daily"
at = "09:30"
[rule.action]
type = "command"
cmd = "echo"
args = ["hi"]

[[rule]]
name = "file-watch"
[rule.when]
type = "file"
path = "/tmp/watched.txt"
op = "changed"
[rule.action]
type = "wake_bit"
bit_url = "http://127.0.0.1:8700"
client_key = "sk-test"
prompt = "check the queue"

[[rule]]
name = "probe"
[rule.when]
type = "http"
url = "http://127.0.0.1:1/health"
expect_status = 204
timeout_secs = 2
[rule.action]
type = "command"
cmd = "true"

[[rule]]
name = "bit-gone"
[rule.when]
type = "process"
name = "bit"
op = "absent"
[rule.action]
type = "command"
cmd = "echo"
args = ["bit gone"]
"#;

    #[test]
    fn parses_all_when_and_action_kinds() {
        let cfg = parse(FULL).expect("full spec must parse");
        assert_eq!(cfg.rules.len(), 5);

        assert_eq!(cfg.rules[0].name, "wake-bit-daily");
        assert_eq!(cfg.rules[0].cooldown_secs, 300);
        assert!(cfg.rules[0].enabled);
        assert_eq!(cfg.rules[0].when, When::Interval { every_secs: 60 });
        assert!(
            matches!(&cfg.rules[0].action, Action::Webhook { url } if url == "http://127.0.0.1:8752/hook")
        );

        assert!(matches!(&cfg.rules[1].when, When::Daily { at } if at == "09:30"));
        assert!(
            matches!(&cfg.rules[1].action, Action::Command { cmd, args } if cmd == "echo" && args == &["hi"])
        );

        assert!(matches!(
            cfg.rules[2].when,
            When::File {
                op: FileOp::Changed,
                ..
            }
        ));
        assert!(
            matches!(&cfg.rules[2].action, Action::WakeBit { client_key, .. } if client_key == "sk-test")
        );

        assert!(matches!(
            cfg.rules[3].when,
            When::Http {
                expect_status: 204,
                timeout_secs: 2,
                ..
            }
        ));

        assert!(matches!(
            cfg.rules[4].when,
            When::Process {
                op: ProcOp::Absent,
                ..
            }
        ));
    }

    #[test]
    fn http_defaults_are_applied() {
        let cfg = parse(
            "[[rule]]\nname = \"h\"\n[rule.when]\ntype = \"http\"\nurl = \"http://x/y\"\n[rule.action]\ntype = \"command\"\ncmd = \"echo\"\n",
        )
        .unwrap();
        match cfg.rules[0].when {
            When::Http {
                expect_status,
                timeout_secs,
                ..
            } => {
                assert_eq!(expect_status, 200);
                assert_eq!(timeout_secs, 5);
            }
            _ => panic!("expected http when"),
        }
    }

    #[test]
    fn lifts_flags_written_after_table_headers() {
        // cooldown_secs/enabled written under [rule.action] must still work
        let cfg = parse(
            "[[rule]]\nname = \"lifted\"\n[rule.when]\ntype = \"interval\"\nevery_secs = 10\n[rule.action]\ntype = \"command\"\ncmd = \"echo\"\ncooldown_secs = 42\nenabled = false\n",
        )
        .unwrap();
        assert_eq!(cfg.rules[0].cooldown_secs, 42);
        assert!(!cfg.rules[0].enabled);
    }

    #[test]
    fn rejects_missing_required_fields() {
        // missing name
        assert!(parse("[[rule]]\n[rule.when]\ntype = \"interval\"\nevery_secs = 5\n[rule.action]\ntype = \"command\"\ncmd = \"echo\"\n").is_err());
        // missing when
        assert!(parse(
            "[[rule]]\nname = \"x\"\n[rule.action]\ntype = \"command\"\ncmd = \"echo\"\n"
        )
        .is_err());
        // missing action
        assert!(parse(
            "[[rule]]\nname = \"x\"\n[rule.when]\ntype = \"interval\"\nevery_secs = 5\n"
        )
        .is_err());
        // http without url
        assert!(parse("[[rule]]\nname = \"x\"\n[rule.when]\ntype = \"http\"\n[rule.action]\ntype = \"command\"\ncmd = \"echo\"\n").is_err());
        // unknown when type
        assert!(parse("[[rule]]\nname = \"x\"\n[rule.when]\ntype = \"magic\"\n[rule.action]\ntype = \"command\"\ncmd = \"echo\"\n").is_err());
        // not TOML at all
        assert!(parse("<<< not toml >>>").is_err());
    }

    #[test]
    fn validate_reports_duplicates_and_bad_values() {
        let cfg = parse(
            "[[rule]]\nname = \"dup\"\n[rule.when]\ntype = \"interval\"\nevery_secs = 0\n[rule.action]\ntype = \"command\"\ncmd = \"echo\"\n\n[[rule]]\nname = \"dup\"\n[rule.when]\ntype = \"daily\"\nat = \"25:99\"\n[rule.action]\ntype = \"webhook\"\nurl = \"ftp://nope\"\n",
        )
        .unwrap();
        let errors = validate(&cfg);
        assert!(
            errors.iter().any(|e| e.contains("duplicate rule name")),
            "errors: {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("every_secs")),
            "errors: {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("daily.at")),
            "errors: {errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("webhook.url")),
            "errors: {errors:?}"
        );
    }

    #[test]
    fn when_serializes_with_type_tag() {
        let cfg = parse(FULL).unwrap();
        let v = serde_json::to_value(&cfg.rules[0].when).unwrap();
        assert_eq!(v["type"], "interval");
        let a = serde_json::to_value(&cfg.rules[2].action).unwrap();
        assert_eq!(a["type"], "wake_bit");
    }

    #[test]
    fn data_dir_respects_env_override() {
        let key = "HOWCUEME_DATA_DIR";
        let saved = std::env::var(key).ok();
        std::env::set_var(key, "/tmp/howcueme-custom-data-dir");
        assert_eq!(
            data_dir().unwrap(),
            PathBuf::from("/tmp/howcueme-custom-data-dir")
        );
        match saved {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}

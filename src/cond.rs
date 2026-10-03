use crate::config::{FileOp, ProcOp};
use chrono::{DateTime, Local, Utc};
use serde_json::{json, Value};
use std::fs;
use std::time::Duration;

/// How long since the last trigger before `interval` fires again.
pub fn eval_interval(last_triggered_secs: Option<i64>, every_secs: u64, now_secs: i64) -> bool {
    match last_triggered_secs {
        None => true,
        Some(last) => now_secs.saturating_sub(last) >= every_secs as i64,
    }
}

/// Daily condition: fires at most once per local calendar day, at or after
/// `at` (HH:MM local time). If the daemon starts later than `at`, it catches
/// up the same day; if it already fired today, it waits for the next day.
pub fn eval_daily(
    at: &chrono::NaiveTime,
    now_local: DateTime<Local>,
    last_triggered_secs: Option<i64>,
) -> bool {
    if now_local.time() < *at {
        return false;
    }
    let today = now_local.date_naive();
    let last_date = last_triggered_secs
        .and_then(|s| DateTime::from_timestamp(s, 0))
        .map(|utc: DateTime<Utc>| utc.with_timezone(&Local).date_naive());
    match last_date {
        Some(d) => d < today,
        None => true,
    }
}

/// Current mtime of `path` in nanoseconds since the Unix epoch.
pub fn file_mtime_nanos(path: &str) -> std::io::Result<i64> {
    let modified = fs::metadata(path)?.modified()?;
    let nanos = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(nanos.min(i64::MAX as u128) as i64)
}

/// File condition.
/// - `exists`: fires whenever the path exists (use `cooldown_secs` to debounce).
/// - `changed`: fires when the mtime differs from the last observed mtime;
///   the first observation only records a baseline and does not fire.
///
/// Returns `(fired, new_snapshot)`; the snapshot is cleared when the file is
/// absent so that a later re-creation is observed as a fresh baseline.
pub fn eval_file(path: &str, op: FileOp, last_mtime_nanos: Option<i64>) -> (bool, Option<i64>) {
    match file_mtime_nanos(path) {
        Ok(nanos) => {
            let fired = match op {
                FileOp::Exists => true,
                FileOp::Changed => last_mtime_nanos.is_some() && last_mtime_nanos != Some(nanos),
            };
            (fired, Some(nanos))
        }
        Err(_) => (false, None),
    }
}

/// HTTP probe: GET `url` and compare the response status with `expect_status`.
pub fn eval_http(url: &str, expect_status: u16, timeout_secs: u64) -> (bool, Value) {
    match ureq::get(url)
        .timeout(Duration::from_secs(timeout_secs.max(1)))
        .call()
    {
        Ok(resp) => {
            let status = resp.status();
            (status == expect_status, json!({ "status": status }))
        }
        Err(ureq::Error::Status(status, _)) => {
            (status == expect_status, json!({ "status": status }))
        }
        Err(e) => (false, json!({ "error": e.to_string() })),
    }
}

/// Process condition: `exists` fires while the process runs, `absent` fires
/// while it does not. Matching is case-insensitive and ignores a `.exe`
/// suffix (Windows friendliness).
pub fn eval_process(name: &str, op: ProcOp) -> (bool, Value) {
    let found = process_running(name);
    let fired = match op {
        ProcOp::Exists => found,
        ProcOp::Absent => !found,
    };
    (fired, json!({ "found": found, "name": name, "op": op }))
}

fn normalize_process_name(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .trim_end_matches(".exe")
        .to_string()
}

pub fn process_running(name: &str) -> bool {
    use sysinfo::{ProcessesToUpdate, System};
    let target = normalize_process_name(name);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes()
        .values()
        .any(|p| normalize_process_name(&p.name().to_string_lossy()) == target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, NaiveTime, TimeZone};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn unique_tmp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "howcueme-cond-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Local HTTP server answering every connection with `status_line`.
    fn spawn_status_server(status_line: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(8) {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let mut buf = [0u8; 1024];
                let _ = Read::read(&mut stream, &mut buf);
                let resp =
                    format!("{status_line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = Write::write_all(&mut stream, resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn interval_fires_first_then_every_period() {
        assert!(eval_interval(None, 60, 1_000));
        assert!(!eval_interval(Some(950), 60, 1_000)); // 50s elapsed < 60s
        assert!(eval_interval(Some(940), 60, 1_000)); // 60s elapsed
        assert!(!eval_interval(Some(1_100), 60, 1_000)); // clock moved back: waits until now catches up
    }

    #[test]
    fn daily_fires_once_per_local_day_with_catch_up() {
        let at = NaiveTime::from_hms_opt(9, 30, 0).unwrap();
        let now = Local.with_ymd_and_hms(2026, 9, 4, 9, 30, 0).unwrap();
        let yesterday = Local
            .with_ymd_and_hms(2026, 9, 3, 20, 0, 0)
            .unwrap()
            .timestamp();
        let today_earlier = Local
            .with_ymd_and_hms(2026, 9, 4, 8, 0, 0)
            .unwrap()
            .timestamp();

        assert!(eval_daily(&at, now, None));
        assert!(eval_daily(&at, now, Some(yesterday)));
        assert!(!eval_daily(&at, now, Some(today_earlier)));

        let early = Local.with_ymd_and_hms(2026, 9, 4, 9, 29, 59).unwrap();
        assert!(!eval_daily(&at, early, None));
    }

    #[test]
    fn file_exists_and_changed_use_mtime_snapshot() {
        let dir = unique_tmp_dir("file");
        let path = dir.join("watch.txt");
        fs::write(&path, b"v1").unwrap();
        let p = path.to_str().unwrap();

        let (fired, snap1) = eval_file(p, FileOp::Exists, None);
        assert!(fired);
        assert!(snap1.is_some());

        // first observation of `changed` records baseline without firing
        let (fired, snap2) = eval_file(p, FileOp::Changed, None);
        assert!(!fired);
        assert_eq!(snap1, snap2);

        std::thread::sleep(Duration::from_millis(30));
        fs::write(&path, b"v2 which is longer").unwrap();
        let (fired, _snap3) = eval_file(p, FileOp::Changed, snap2);
        assert!(fired);

        fs::remove_file(&path).unwrap();
        let (fired, snap4) = eval_file(p, FileOp::Changed, snap2);
        assert!(!fired);
        assert!(snap4.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn http_condition_matches_expected_status() {
        let url = spawn_status_server("HTTP/1.1 204 No Content");
        let (fired, result) = eval_http(&url, 204, 5);
        assert!(fired);
        assert_eq!(result["status"], 204);
        let (fired, result) = eval_http(&url, 200, 5);
        assert!(!fired);
        assert_eq!(result["status"], 204);
    }

    #[test]
    fn http_condition_reports_transport_errors() {
        // nothing listens here; must not fire and must not panic
        let (fired, result) = eval_http("http://127.0.0.1:9/unreachable", 200, 1);
        assert!(!fired);
        assert!(result.get("error").is_some() || result.get("status").is_some());
    }

    #[test]
    fn process_condition_absent_fires_for_missing_process() {
        let (fired, result) = eval_process("howcueme-definitely-not-running-9f3a1", ProcOp::Absent);
        assert!(fired);
        assert_eq!(result["found"], false);
        let (fired, _) = eval_process("howcueme-definitely-not-running-9f3a1", ProcOp::Exists);
        assert!(!fired);
    }

    #[test]
    fn file_condition_handles_traversal_and_missing_paths_safely() {
        // 配置里的 file.path 是不可信输入：指向不存在/穿越路径时只报告不触发、不 panic。
        let (fired, snap) = eval_file("../../../../no/such/file-9f3a1.txt", FileOp::Exists, None);
        assert!(!fired, "不存在的路径不得触发");
        assert!(snap.is_none());
        let (fired, snap) = eval_file("..\\..\\windows\\system32\\no-such-9f3a1", FileOp::Changed, Some(1));
        assert!(!fired);
        assert!(snap.is_none());
    }
}

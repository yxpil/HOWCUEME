use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Per-rule persisted state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_triggered_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_mtime_nanos: Option<i64>,
}

/// Content of `<data_dir>/state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateFile {
    #[serde(default)]
    pub rules: BTreeMap<String, RuleState>,
}

impl StateFile {
    /// Load state; missing or corrupt files yield an empty state.
    pub fn load(path: &Path) -> StateFile {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Atomically persist state (write temp file, then rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip_and_missing_file() {
        let dir = std::env::temp_dir().join(format!(
            "howcueme-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("state.json");

        // missing file -> empty state
        let st = StateFile::load(&path);
        assert!(st.rules.is_empty());

        let mut st = StateFile::default();
        st.rules.insert(
            "wake-bit-daily".to_string(),
            RuleState {
                last_triggered_secs: Some(1_700_000_000),
                last_mtime_nanos: Some(42),
            },
        );
        st.save(&path).expect("save must succeed");

        let loaded = StateFile::load(&path);
        assert_eq!(
            loaded.rules["wake-bit-daily"].last_triggered_secs,
            Some(1_700_000_000)
        );
        assert_eq!(loaded.rules["wake-bit-daily"].last_mtime_nanos, Some(42));

        // corrupt file -> empty state, no panic
        fs::write(&path, "not json{").unwrap();
        let st = StateFile::load(&path);
        assert!(st.rules.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }
}

//! TalaDB config loader.
//!
//! Parses and validates `taladb.config.yml` / `taladb.config.json`.
//!
//! Engine-level settings only. The change-webhook configuration lives in the
//! `taladb` TypeScript client, which owns webhook delivery on every runtime —
//! see `packages/clients/taladb/src/config.ts`. Unknown top-level keys are
//! ignored, so one config file can carry both and be read by either.
//!
//! # File format
//!
//! ```yaml
//! durability:
//!   flush_every_write: false
//!   flush_ms: 250
//! ```
//!
//! or equivalently in JSON:
//!
//! ```json
//! { "durability": { "flush_every_write": false, "flush_ms": 250 } }
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::TalaDbError;

// ---------------------------------------------------------------------------
// Config structs
// ---------------------------------------------------------------------------

/// Top-level TalaDB configuration (from `taladb.config.yml` / `taladb.config.json`).
///
/// Unknown top-level keys are silently ignored so future config additions are
/// backwards-compatible with older library versions.
/// Storage durability configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DurabilityConfig {
    /// When `true` (default), every write commit is fsync'd immediately (redb
    /// `Immediate` durability) — a crash never loses an acknowledged write. When
    /// `false`, commits are batched (redb `Eventual`) for higher write
    /// throughput; call `db.flush()` to force a durable sync. Applies to
    /// file-backed (Node) and OPFS (browser) storage; in-memory ignores it.
    #[serde(default = "default_true")]
    pub flush_every_write: bool,
    /// Browser IndexedDB-fallback snapshot debounce, in milliseconds. `None`
    /// uses the engine default (500 ms). Only affects the non-OPFS browser
    /// fallback path — the OPFS and Node paths use `flush_every_write` instead.
    #[serde(default)]
    pub flush_ms: Option<u32>,
}

fn default_true() -> bool {
    true
}

impl Default for DurabilityConfig {
    fn default() -> Self {
        Self {
            flush_every_write: true,
            flush_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TalaDbConfig {
    /// Storage durability configuration.
    #[serde(default)]
    pub durability: DurabilityConfig,
}

// ---------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------

/// Load and validate a config from an explicit file path.
///
/// Supported extensions: `.yml`, `.yaml`, `.json`.
///
/// Returns `Err(TalaDbError::Config(...))` if the file cannot be read,
/// cannot be parsed, or fails validation.
pub fn load_from_path(path: &Path) -> Result<TalaDbConfig, TalaDbError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        TalaDbError::Config(format!("failed to read config at {}: {e}", path.display()))
    })?;

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let config: TalaDbConfig = match ext {
        #[cfg(feature = "config-yaml")]
        "yml" | "yaml" => serde_yaml::from_str(&content)
            .map_err(|e| TalaDbError::Config(format!("invalid YAML config: {e}")))?,
        #[cfg(not(feature = "config-yaml"))]
        "yml" | "yaml" => {
            return Err(TalaDbError::Config(
                "YAML config support is not compiled into this build \
                 (enable the `config-yaml` feature) — use .json instead"
                    .into(),
            ));
        }
        "json" => serde_json::from_str(&content)
            .map_err(|e| TalaDbError::Config(format!("invalid JSON config: {e}")))?,
        other => {
            return Err(TalaDbError::Config(format!(
                "unsupported config extension \".{other}\" — use .yml, .yaml, or .json"
            )));
        }
    };

    Ok(config)
}

/// Auto-discover a config from `cwd`.
///
/// Searches for `taladb.config.yml`, `taladb.config.yaml`, then
/// `taladb.config.json` (in that order) inside the given directory.
///
/// If no config file is found, returns the default config —
/// **not an error**. The database works normally without a config file.
pub fn load_auto(cwd: &Path) -> Result<TalaDbConfig, TalaDbError> {
    for name in [
        "taladb.config.yml",
        "taladb.config.yaml",
        "taladb.config.json",
    ] {
        let path = cwd.join(name);
        if path.exists() {
            return load_from_path(&path);
        }
    }
    Ok(TalaDbConfig::default())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // ── parse helpers ────────────────────────────────────────────────────────

    fn write_tmp(content: &str, ext: &str) -> NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(&format!(".{ext}"))
            .tempfile()
            .unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    // ── YAML ─────────────────────────────────────────────────────────────────

    #[test]
    fn parses_valid_yaml() {
        let f = write_tmp(
            r#"
durability:
  flush_every_write: false
  flush_ms: 250
"#,
            "yml",
        );
        let cfg = load_from_path(f.path()).unwrap();
        assert!(!cfg.durability.flush_every_write);
        assert_eq!(cfg.durability.flush_ms, Some(250));
    }

    #[test]
    fn parses_yaml_with_yaml_extension() {
        let f = write_tmp("durability:\n  flush_every_write: false\n", "yaml");
        let cfg = load_from_path(f.path()).unwrap();
        assert!(!cfg.durability.flush_every_write);
    }

    #[test]
    fn parses_valid_json() {
        let f = write_tmp(
            r#"{ "durability": { "flush_every_write": false, "flush_ms": 100 } }"#,
            "json",
        );
        let cfg = load_from_path(f.path()).unwrap();
        assert!(!cfg.durability.flush_every_write);
        assert_eq!(cfg.durability.flush_ms, Some(100));
    }

    #[test]
    fn flush_every_write_defaults_to_true() {
        // Durability must default to the safe setting: a config that omits the
        // key entirely, and one that omits only `flush_every_write`, both fsync.
        let f = write_tmp("durability:\n  flush_ms: 50\n", "yml");
        let cfg = load_from_path(f.path()).unwrap();
        assert!(cfg.durability.flush_every_write);
        assert_eq!(cfg.durability.flush_ms, Some(50));

        let empty = write_tmp("{}", "json");
        assert!(
            load_from_path(empty.path())
                .unwrap()
                .durability
                .flush_every_write
        );
    }

    #[test]
    fn defaults_when_no_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_auto(dir.path()).unwrap();
        assert_eq!(cfg, TalaDbConfig::default());
        assert!(cfg.durability.flush_every_write);
    }

    #[test]
    fn load_auto_finds_yml_before_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("taladb.config.yml"),
            "durability:\n  flush_ms: 111\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("taladb.config.json"),
            r#"{"durability":{"flush_ms":222}}"#,
        )
        .unwrap();
        let cfg = load_auto(dir.path()).unwrap();
        assert_eq!(cfg.durability.flush_ms, Some(111));
    }

    #[test]
    fn ignores_unknown_keys() {
        // The `webhook` block belongs to the TypeScript client, which reads the
        // same file. The engine reader must skip it rather than reject the file.
        let f = write_tmp(
            r#"
durability:
  flush_every_write: true
webhook:
  enabled: true
  endpoint: "https://api.example.com/hook"
unknown_top_level: "ignored"
"#,
            "yml",
        );
        let cfg = load_from_path(f.path()).unwrap();
        assert!(cfg.durability.flush_every_write);
    }

    // ── errors ───────────────────────────────────────────────────────────────

    #[test]
    fn returns_error_on_unreadable_file() {
        let err = load_from_path(Path::new("/nonexistent/taladb.config.yml")).unwrap_err();
        assert!(matches!(err, TalaDbError::Config(_)));
    }

    #[test]
    fn returns_error_on_unsupported_extension() {
        let f = write_tmp("durability:\n  flush_ms: 1\n", "toml");
        let err = load_from_path(f.path()).unwrap_err().to_string();
        assert!(err.contains("unsupported config extension"), "{err}");
    }
}

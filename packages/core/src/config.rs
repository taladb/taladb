//! TalaDB config loader.
//!
//! Parses and validates `taladb.config.yml` / `taladb.config.json`.
//! The config is available internally after `Phase 1` but drives no behaviour
//! until the HTTP sync adapter is wired in Phase 3.
//!
//! # File format
//!
//! ```yaml
//! sync:
//!   enabled: true
//!   endpoint: "https://api.example.com/taladb-events"
//!   headers:
//!     Authorization: "Bearer my-token"
//! ```
//!
//! or equivalently in JSON:
//!
//! ```json
//! {
//!   "sync": {
//!     "enabled": true,
//!     "endpoint": "https://api.example.com/taladb-events",
//!     "headers": { "Authorization": "Bearer my-token" }
//!   }
//! }
//! ```

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::TalaDbError;

// Localhost variants accepted for plaintext HTTP without a warning.
const LOCALHOST_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]", "::1"];

/// Split an absolute HTTP(S) endpoint into `(scheme, host)`, or `None` if it is
/// not one.
///
/// A full URL parser is deliberately not used here. `url` drags in `idna` and
/// its Unicode tables, which is a large, permanent addition to a WASM bundle
/// that is downloaded and compiled on every cold start — for a config sanity
/// check that only ever needed the scheme and the host. The endpoint is handed
/// to `fetch`/`reqwest` afterwards, which does parse it properly; this is a
/// fail-fast on obviously wrong config, not a security boundary.
///
/// `host` excludes any port and userinfo, and keeps the brackets on an IPv6
/// literal, matching what `Url::host_str` returned.
///
/// The `Err` is the reason fragment appended to the error message. The two
/// variants are kept distinct because callers assert on them: a wrong scheme
/// and a missing host are different mistakes and were reported differently
/// before this function replaced `url::Url::parse`.
fn split_http_endpoint(raw: &str) -> Result<(&str, &str), &'static str> {
    const BAD_SCHEME: &str = "must start with http:// or https://";
    const MISSING_HOST: &str = "missing host";

    let (scheme, rest) = raw.split_once("://").ok_or(BAD_SCHEME)?;
    if scheme != "http" && scheme != "https" {
        return Err(BAD_SCHEME);
    }
    // Authority runs up to the first '/', '?' or '#'.
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|a| !a.is_empty())
        .ok_or(MISSING_HOST)?;
    // Drop userinfo (`user:pass@host`); the last '@' wins, as in RFC 3986.
    let hostport = match authority.rsplit_once('@') {
        Some((_, after)) => after,
        None => authority,
    };
    // An IPv6 literal is bracketed, and its colons are not port separators.
    let host = if let Some(close) = hostport.find(']') {
        if !hostport.starts_with('[') {
            return Err(MISSING_HOST);
        }
        &hostport[..=close]
    } else {
        hostport.split(':').next().ok_or(MISSING_HOST)?
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(MISSING_HOST);
    }
    Ok((scheme, host))
}

// ---------------------------------------------------------------------------
// Config structs
// ---------------------------------------------------------------------------

/// HTTP push sync settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SyncConfig {
    /// Enable HTTP push sync. Defaults to `false` — everything is a no-op when
    /// disabled, so adding the config block without `enabled: true` is safe.
    #[serde(default)]
    pub enabled: bool,

    /// Default endpoint URL that receives all mutation events (`insert`, `update`,
    /// `delete`). Required when `enabled: true`; ignored otherwise.
    pub endpoint: Option<String>,

    /// HTTP headers sent with every outgoing request.
    /// Typical use: `Authorization: "Bearer <token>"`.
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Override the endpoint for `insert` events only.
    pub insert_endpoint: Option<String>,

    /// Override the endpoint for `update` events only.
    pub update_endpoint: Option<String>,

    /// Override the endpoint for `delete` events only.
    pub delete_endpoint: Option<String>,

    /// Document fields to omit from every outgoing sync payload.
    ///
    /// Useful for stripping large computed fields such as embedding vectors
    /// that the remote endpoint doesn't need and shouldn't pay to transmit.
    ///
    /// Fields listed here are silently ignored if they are absent from the
    /// document — no error is raised.
    #[serde(default)]
    pub exclude_fields: Vec<String>,
}

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
        DurabilityConfig {
            flush_every_write: true,
            flush_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TalaDbConfig {
    /// HTTP push sync configuration. Disabled by default.
    #[serde(default)]
    pub sync: SyncConfig,
    /// Storage durability configuration.
    #[serde(default)]
    pub durability: DurabilityConfig,
}

impl TalaDbConfig {
    /// Validate a parsed config.
    ///
    /// Checks that every endpoint URL (if present) is a well-formed HTTP or
    /// HTTPS URL. Returns `Err(TalaDbError::Config(...))` on the first invalid value.
    pub fn validate(&self) -> Result<(), TalaDbError> {
        for raw in [
            self.sync.endpoint.as_deref(),
            self.sync.insert_endpoint.as_deref(),
            self.sync.update_endpoint.as_deref(),
            self.sync.delete_endpoint.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let (scheme, host) = split_http_endpoint(raw).map_err(|reason| {
                TalaDbError::Config(format!("invalid endpoint URL \"{raw}\" — {reason}"))
            })?;
            // Warn when a non-localhost HTTP endpoint is used — changesets are
            // transmitted without encryption and are vulnerable to interception.
            if scheme == "http" && !LOCALHOST_HOSTS.contains(&host) {
                tracing::warn!(
                    endpoint = raw,
                    "taladb: sync endpoint uses plaintext HTTP — \
                     use HTTPS in production to prevent changeset interception"
                );
            }
        }
        Ok(())
    }
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

    config.validate()?;
    Ok(config)
}

/// Auto-discover a config from `cwd`.
///
/// Searches for `taladb.config.yml`, `taladb.config.yaml`, then
/// `taladb.config.json` (in that order) inside the given directory.
///
/// If no config file is found, returns a default (sync-disabled) config —
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
sync:
  enabled: true
  endpoint: "https://api.example.com/hook"
  headers:
    Authorization: "Bearer token123"
    X-Custom: "value"
"#,
            "yml",
        );
        let cfg = load_from_path(f.path()).unwrap();
        assert!(cfg.sync.enabled);
        assert_eq!(
            cfg.sync.endpoint.as_deref(),
            Some("https://api.example.com/hook")
        );
        assert_eq!(
            cfg.sync.headers.get("Authorization").map(String::as_str),
            Some("Bearer token123")
        );
        assert_eq!(
            cfg.sync.headers.get("X-Custom").map(String::as_str),
            Some("value")
        );
    }

    #[test]
    fn parses_yaml_with_yaml_extension() {
        let f = write_tmp("sync:\n  enabled: false\n", "yaml");
        let cfg = load_from_path(f.path()).unwrap();
        assert!(!cfg.sync.enabled);
    }

    #[test]
    fn parses_valid_json() {
        let f = write_tmp(
            r#"{
  "sync": {
    "enabled": true,
    "endpoint": "http://localhost:4000/events",
    "headers": { "Authorization": "Bearer tok" }
  }
}"#,
            "json",
        );
        let cfg = load_from_path(f.path()).unwrap();
        assert!(cfg.sync.enabled);
        assert_eq!(
            cfg.sync.endpoint.as_deref(),
            Some("http://localhost:4000/events")
        );
    }

    #[test]
    fn defaults_when_no_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_auto(dir.path()).unwrap();
        assert_eq!(cfg, TalaDbConfig::default());
        assert!(!cfg.sync.enabled);
    }

    #[test]
    fn load_auto_finds_yml_before_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("taladb.config.yml"),
            "sync:\n  enabled: true\n  endpoint: \"https://yml.example.com\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("taladb.config.json"),
            r#"{"sync":{"enabled":true,"endpoint":"https://json.example.com"}}"#,
        )
        .unwrap();
        let cfg = load_auto(dir.path()).unwrap();
        assert_eq!(
            cfg.sync.endpoint.as_deref(),
            Some("https://yml.example.com")
        );
    }

    #[test]
    fn ignores_unknown_keys() {
        let f = write_tmp(
            r#"
sync:
  enabled: false
unknown_top_level: "ignored"
"#,
            "yml",
        );
        // Should not error — unknown keys are silently ignored.
        assert!(load_from_path(f.path()).is_ok());
    }

    // ── validation ───────────────────────────────────────────────────────────

    #[test]
    fn rejects_non_http_endpoint() {
        let f = write_tmp(
            "sync:\n  enabled: true\n  endpoint: \"ftp://wrong.example.com\"\n",
            "yml",
        );
        let err = load_from_path(f.path()).unwrap_err();
        assert!(err.to_string().contains("invalid endpoint URL"));
    }

    #[test]
    fn rejects_relative_endpoint() {
        let f = write_tmp(
            "sync:\n  enabled: true\n  endpoint: \"/relative/path\"\n",
            "yml",
        );
        let err = load_from_path(f.path()).unwrap_err();
        assert!(err.to_string().contains("invalid endpoint URL"));
    }

    /// The wording of these two messages is a public contract — the React
    /// Native binding asserts on the scheme message, and it broke when this
    /// validation stopped using `url::Url::parse` and collapsed every failure
    /// into one generic string. Pin both here so core catches it first.
    #[test]
    fn endpoint_errors_distinguish_scheme_from_host() {
        let bad_scheme = TalaDbConfig {
            sync: SyncConfig {
                enabled: true,
                endpoint: Some("file:///not-http".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = bad_scheme.validate().unwrap_err().to_string();
        assert!(
            err.contains("must start with http:// or https://"),
            "got: {err}"
        );

        let no_host = TalaDbConfig {
            sync: SyncConfig {
                enabled: true,
                endpoint: Some("https:///path-only".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = no_host.validate().unwrap_err().to_string();
        assert!(err.contains("missing host"), "got: {err}");
    }

    #[test]
    fn validates_per_event_endpoints() {
        let f = write_tmp(
            r#"
sync:
  enabled: true
  endpoint: "https://api.example.com/all"
  insert_endpoint: "not-a-url"
"#,
            "yml",
        );
        let err = load_from_path(f.path()).unwrap_err();
        assert!(err.to_string().contains("invalid endpoint URL"));
    }

    #[test]
    fn accepts_http_and_https_endpoints() {
        let cfg = TalaDbConfig {
            sync: SyncConfig {
                enabled: true,
                endpoint: Some("https://secure.example.com".into()),
                insert_endpoint: Some("http://localhost:3000/insert".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn returns_error_on_unreadable_file() {
        let err = load_from_path(Path::new("/nonexistent/taladb.config.yml")).unwrap_err();
        assert!(err.to_string().contains("failed to read config"));
    }

    #[test]
    fn returns_error_on_unsupported_extension() {
        let f = write_tmp("sync:\n  enabled: true\n", "toml");
        let err = load_from_path(f.path()).unwrap_err();
        assert!(err.to_string().contains("unsupported config extension"));
    }

    #[test]
    fn parses_exclude_fields() {
        let f = write_tmp(
            r#"
sync:
  enabled: true
  endpoint: "https://api.example.com/events"
  exclude_fields:
    - embedding
    - clip_vector
    - internal_score
"#,
            "yml",
        );
        let cfg = load_from_path(f.path()).unwrap();
        assert_eq!(
            cfg.sync.exclude_fields,
            vec!["embedding", "clip_vector", "internal_score"]
        );
    }

    #[test]
    fn exclude_fields_defaults_to_empty() {
        let f = write_tmp(
            "sync:\n  enabled: true\n  endpoint: \"https://api.example.com\"\n",
            "yml",
        );
        let cfg = load_from_path(f.path()).unwrap();
        assert!(cfg.sync.exclude_fields.is_empty());
    }

    // ── split_http_endpoint ─────────────────────────────────────────────
    //
    // These pin the behaviour that replaced `url::Url::parse`, including the
    // shapes a hand-rolled split is most likely to get wrong.

    #[test]
    fn endpoint_split_extracts_scheme_and_host() {
        for (raw, want) in [
            ("https://api.example.com", ("https", "api.example.com")),
            ("http://localhost:3000/insert", ("http", "localhost")),
            ("https://api.example.com/a/b?x=1#f", ("https", "api.example.com")),
            ("https://user:pw@api.example.com/p", ("https", "api.example.com")),
            ("http://127.0.0.1:8080", ("http", "127.0.0.1")),
            ("http://[::1]:9000/sync", ("http", "[::1]")),
            ("https://api.example.com?q=1", ("https", "api.example.com")),
        ] {
            assert_eq!(split_http_endpoint(raw), Ok(want), "for {raw}");
        }
    }

    #[test]
    fn endpoint_split_rejects_non_http_and_hostless() {
        // Paired with the reason each one is rejected for, so the two error
        // messages stay distinguishable — see
        // `endpoint_errors_distinguish_scheme_from_host`.
        for (raw, want_reason) in [
            ("ftp://wrong.example.com", "must start with"),
            ("/relative/path", "must start with"),
            ("not-a-url", "must start with"),
            ("://no-scheme", "must start with"),
            ("", "must start with"),
            ("https://", "missing host"),
            ("https:///path-only", "missing host"),
            ("https://has space/x", "missing host"),
        ] {
            let err = split_http_endpoint(raw).unwrap_err();
            assert!(
                err.contains(want_reason),
                "for {raw}: expected {want_reason:?}, got {err:?}"
            );
        }
    }

    /// The localhost allowlist is matched against the extracted host, so a
    /// plaintext loopback endpoint must not be flagged while a remote one is.
    #[test]
    fn endpoint_split_host_feeds_localhost_allowlist() {
        let (_, host) = split_http_endpoint("http://localhost:3000/x").unwrap();
        assert!(LOCALHOST_HOSTS.contains(&host));
        let (_, host) = split_http_endpoint("http://evil.example.com/x").unwrap();
        assert!(!LOCALHOST_HOSTS.contains(&host));
    }
}

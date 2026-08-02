//! TalaDB CLI — dev tools for TalaDB databases.
//!
//! Commands:
//!   taladb inspect <file>                       — print DB stats
//!   taladb export  <file> <collection> [--fmt]  — export to JSON / NDJSON / CSV
//!   taladb import  <file> <collection> <data>   — import from JSON / NDJSON
//!   taladb collections <file>                   — list all collections
//!   taladb count   <file> <collection>          — count documents
//!   taladb drop    <file> <collection>          — drop an entire collection
//!   taladb sync    <file> [collection]           — push all docs to configured HTTP endpoint
//!   taladb studio  <file> [--port]               — start a local web UI

mod studio;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use taladb_core::{Database, Document, Filter, Value};

// ---------------------------------------------------------------------------
// CLI argument types
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "taladb",
    version,
    about = "TalaDB command-line dev tools",
    long_about = "Inspect, export, and import data in TalaDB database files."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print database statistics (collections, document counts, index info).
    Inspect {
        /// Path to the TalaDB database file.
        file: PathBuf,
    },

    /// List all collection names in the database.
    Collections {
        /// Path to the TalaDB database file.
        file: PathBuf,
    },

    /// Count documents in a collection.
    Count {
        /// Path to the TalaDB database file.
        file: PathBuf,
        /// Collection name.
        collection: String,
    },

    /// Export a collection to JSON, NDJSON, or CSV.
    Export {
        /// Path to the TalaDB database file.
        file: PathBuf,
        /// Collection name to export.
        collection: String,
        /// Output format.
        #[arg(long, short, default_value = "json")]
        fmt: ExportFormat,
        /// Output file path. Defaults to stdout.
        #[arg(long, short)]
        out: Option<PathBuf>,
    },

    /// Import documents from a JSON array or NDJSON file into a collection.
    Import {
        /// Path to the TalaDB database file (created if it doesn't exist).
        file: PathBuf,
        /// Collection name to import into.
        collection: String,
        /// Input data file (.json array or .ndjson).
        data: PathBuf,
    },

    /// Delete all documents in a collection (does NOT drop indexes).
    Drop {
        /// Path to the TalaDB database file.
        file: PathBuf,
        /// Collection name to clear.
        collection: String,
    },

    /// Rebuild the HNSW graph for a vector index from the current flat data.
    ///
    /// Use after bulk inserts or when approximate-nearest-neighbor recall has
    /// degraded due to writes since the graph was last built.
    /// No-op when the vector-hnsw feature is disabled or the index is flat-only.
    UpgradeVectorIndex {
        /// Path to the TalaDB database file.
        file: PathBuf,
        /// Collection name.
        collection: String,
        /// Vector field name.
        field: String,
    },

    /// Start a local web UI for browsing and editing the database.
    ///
    /// Serves a browser-based studio on http://localhost:<port> (default 4321).
    /// Opens a browser window automatically unless --no-open is passed.
    ///
    /// Example:
    ///   taladb studio myapp.db
    ///   taladb studio myapp.db --port 8080
    Studio {
        /// Path to the TalaDB database file.
        file: PathBuf,
        /// Port to listen on (default: 4321).
        #[arg(long, default_value_t = 4321)]
        port: u16,
        /// Address to bind. Defaults to loopback only; the studio has no
        /// authentication, so binding 0.0.0.0 exposes read/delete access to
        /// the whole network.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Do not open a browser window automatically.
        #[arg(long)]
        no_open: bool,
    },

    /// Push all local documents to the configured HTTP endpoint.
    ///
    /// Reads `taladb.config.yml` / `taladb.config.json` from the database
    /// file's parent directory (or use `--config` for an explicit path).
    /// Fires one HTTP POST per document with `"_taladb_event": "insert"`.
    ///
    /// Example:
    ///   taladb sync myapp.db
    ///   taladb sync myapp.db articles --dry-run
    Sync {
        /// Path to the TalaDB database file.
        file: PathBuf,
        /// Collection name to sync. Syncs all collections when omitted.
        collection: Option<String>,
        /// Print events as JSON without sending any HTTP requests.
        #[arg(long)]
        dry_run: bool,
        /// Explicit path to a config file. Auto-discovers from the database
        /// file's directory when omitted.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

#[derive(Clone, ValueEnum)]
enum ExportFormat {
    /// Pretty-printed JSON array.
    Json,
    /// Newline-delimited JSON (one document per line).
    Ndjson,
    /// Comma-separated values (flat fields only).
    Csv,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Reject paths containing `..` components to prevent directory traversal.
///
/// The CLI runs with the user's own permissions so the OS enforces access
/// control, but rejecting `..` paths avoids accidental overwrites of files
/// outside the intended working directory.
fn check_no_traversal(path: &std::path::Path, label: &str) -> Result<()> {
    if path
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        anyhow::bail!(
            "{label} path must not contain '..' components: {}",
            path.display()
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Inspect { file } => cmd_inspect(&file),
        Command::Collections { file } => cmd_collections(&file),
        Command::Count { file, collection } => cmd_count(&file, &collection),
        Command::Export {
            file,
            collection,
            fmt,
            out,
        } => cmd_export(&file, &collection, fmt, out.as_deref()),
        Command::Import {
            file,
            collection,
            data,
        } => cmd_import(&file, &collection, &data),
        Command::Drop { file, collection } => cmd_drop(&file, &collection),
        Command::UpgradeVectorIndex {
            file,
            collection,
            field,
        } => cmd_upgrade_vector_index(&file, &collection, &field),
        Command::Studio {
            file,
            port,
            host,
            no_open,
        } => studio::cmd_studio(&file, port, &host, no_open),
        Command::Sync {
            file,
            collection,
            dry_run,
            config,
        } => cmd_sync(&file, collection.as_deref(), dry_run, config.as_deref()),
    }
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

fn cmd_inspect(file: &PathBuf) -> Result<()> {
    let db = Database::open(file).with_context(|| format!("opening {file:?}"))?;

    println!("TalaDB Inspector");
    println!("────────────────");
    println!("File: {}", file.display());

    let collections = db.list_collection_names()?;
    if collections.is_empty() {
        println!("\nNo collections found.");
    } else {
        println!("\nCollections ({}):", collections.len());
        for name in &collections {
            let n = db.collection(name)?.count(Filter::All)?;
            println!("  {name}  ({n} documents)");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// collections
// ---------------------------------------------------------------------------

fn cmd_collections(file: &PathBuf) -> Result<()> {
    let db = Database::open(file).with_context(|| format!("opening {file:?}"))?;
    let collections = db.list_collection_names()?;
    for name in &collections {
        println!("{name}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// count
// ---------------------------------------------------------------------------

fn cmd_count(file: &PathBuf, collection: &str) -> Result<()> {
    let db = Database::open(file).with_context(|| format!("opening {file:?}"))?;
    let col = db.collection(collection)?;
    let n = col.count(Filter::All)?;
    println!("{n}");
    Ok(())
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

fn cmd_export(
    file: &PathBuf,
    collection: &str,
    fmt: ExportFormat,
    out: Option<&std::path::Path>,
) -> Result<()> {
    let db = Database::open(file).with_context(|| format!("opening {file:?}"))?;
    let col = db.collection(collection)?;
    let docs = col.find(Filter::All)?;

    let json_docs: Vec<serde_json::Value> = docs
        .iter()
        .map(|doc| {
            let mut map = serde_json::Map::new();
            map.insert("_id".into(), serde_json::Value::String(doc.id.to_string()));
            for (k, v) in &doc.fields {
                map.insert(k.clone(), value_to_json(v));
            }
            serde_json::Value::Object(map)
        })
        .collect();

    let output: String = match fmt {
        ExportFormat::Json => serde_json::to_string_pretty(&json_docs)?,
        ExportFormat::Ndjson => json_docs
            .iter()
            .map(|d| serde_json::to_string(d).unwrap())
            .collect::<Vec<_>>()
            .join("\n"),
        ExportFormat::Csv => {
            if json_docs.is_empty() {
                String::new()
            } else {
                // Collect all field names from the first document
                let headers: Vec<String> = if let serde_json::Value::Object(m) = &json_docs[0] {
                    m.keys().cloned().collect()
                } else {
                    vec![]
                };
                let mut rows = vec![headers.join(",")];
                for doc in &json_docs {
                    if let serde_json::Value::Object(m) = doc {
                        let row: Vec<String> = headers
                            .iter()
                            .map(|h| m.get(h).map(csv_escape).unwrap_or_default())
                            .collect();
                        rows.push(row.join(","));
                    }
                }
                rows.join("\n")
            }
        }
    };

    match out {
        Some(path) => {
            check_no_traversal(path, "--out")?;
            std::fs::write(path, &output).with_context(|| format!("writing {path:?}"))?
        }
        None => println!("{output}"),
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

fn cmd_import(file: &PathBuf, collection: &str, data: &PathBuf) -> Result<()> {
    check_no_traversal(data, "import data")?;
    let db = Database::open(file).with_context(|| format!("opening {file:?}"))?;
    let col = db.collection(collection)?;

    let content = std::fs::read_to_string(data).with_context(|| format!("reading {data:?}"))?;

    // Detect format: NDJSON (one JSON object per line) vs JSON array
    let docs: Vec<serde_json::Value> = if content.trim_start().starts_with('[') {
        serde_json::from_str(&content)?
    } else {
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).map_err(anyhow::Error::from))
            .collect::<Result<Vec<_>>>()?
    };

    let count = docs.len();
    let items: Vec<Vec<(String, Value)>> = docs
        .into_iter()
        .map(|d| {
            if let serde_json::Value::Object(map) = d {
                map.into_iter()
                    .filter(|(k, _)| k != "_id") // skip imported IDs — new ULIDs assigned
                    .map(|(k, v)| (k, json_to_value(v)))
                    .collect()
            } else {
                vec![]
            }
        })
        .collect();

    col.insert_many(items)?;
    eprintln!("Imported {count} documents into '{collection}'");
    Ok(())
}

// ---------------------------------------------------------------------------
// drop
// ---------------------------------------------------------------------------

fn cmd_drop(file: &PathBuf, collection: &str) -> Result<()> {
    let db = Database::open(file).with_context(|| format!("opening {file:?}"))?;
    let col = db.collection(collection)?;
    let n = col.delete_many(Filter::All)?;
    eprintln!("Deleted {n} documents from '{collection}'");
    Ok(())
}

// ---------------------------------------------------------------------------
// upgrade-vector-index
// ---------------------------------------------------------------------------

fn cmd_upgrade_vector_index(file: &PathBuf, collection: &str, field: &str) -> Result<()> {
    let db = Database::open(file).with_context(|| format!("opening {file:?}"))?;
    db.collection(collection)?
        .upgrade_vector_index(field)
        .with_context(|| {
            format!(
                "rebuilding HNSW graph for '{collection}::{field}' in {file:?}"
            )
        })?;
    eprintln!("HNSW graph for '{collection}::{field}' rebuilt successfully.");
    Ok(())
}

// ---------------------------------------------------------------------------
// sync
// ---------------------------------------------------------------------------

fn cmd_sync(
    file: &std::path::Path,
    collection: Option<&str>,
    dry_run: bool,
    config_path: Option<&std::path::Path>,
) -> Result<()> {
    // Load config — auto-discover from the database file's parent directory.
    let cfg = match config_path {
        Some(p) => {
            check_no_traversal(p, "--config")?;
            taladb_core::load_from_path(p)
                .with_context(|| format!("loading config from {p:?}"))?
        }
        None => {
            let dir = file.parent().unwrap_or(std::path::Path::new("."));
            taladb_core::load_auto(dir)?
        }
    };

    if !cfg.sync.enabled {
        eprintln!("Sync is disabled (sync.enabled: false in config). Nothing to do.");
        return Ok(());
    }

    // Resolve the insert endpoint: per-event override takes precedence.
    let endpoint = cfg
        .sync
        .insert_endpoint
        .as_deref()
        .or(cfg.sync.endpoint.as_deref())
        .ok_or_else(|| anyhow::anyhow!("sync.endpoint is required when sync.enabled: true"))?
        .to_string();

    let db = Database::open(file).with_context(|| format!("opening {file:?}"))?;

    let all_names = db.list_collection_names()?;
    let col_names: Vec<&str> = match collection {
        Some(c) => vec![c],
        None => all_names.iter().map(String::as_str).collect(),
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Build the HTTP client once (skipped for dry-run).
    let client = if !dry_run {
        Some(
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .context("building HTTP client")?,
        )
    } else {
        None
    };

    let mut total_sent = 0u64;

    for col_name in &col_names {
        let docs = db.collection(col_name)?.find(Filter::All)?;
        let total = docs.len();

        if total == 0 {
            eprintln!("Syncing {col_name}... 0/0 (empty)");
            continue;
        }

        for (idx, doc) in docs.iter().enumerate() {
            let n = idx + 1;
            let payload = sync_payload(col_name, doc, now_ms, &cfg.sync.exclude_fields);

            if dry_run {
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                eprint!("\rSyncing {col_name}... {n}/{total} ");
                http_post_with_retry(
                    client.as_ref().unwrap(),
                    &endpoint,
                    &cfg.sync.headers,
                    &payload,
                )
                .with_context(|| format!("sending event for {col_name}/{}", doc.id))?;
            }

            total_sent += 1;
        }

        if !dry_run {
            eprintln!("\rSyncing {col_name}... {total}/{total} \u{2713}");
        }
    }

    if !dry_run {
        eprintln!("Done. {total_sent} event(s) sent.");
    }

    Ok(())
}

/// Build the `insert` event payload for a single document, omitting any field
/// listed in `exclude`.
fn sync_payload(
    collection: &str,
    doc: &Document,
    timestamp: u64,
    exclude: &[String],
) -> serde_json::Value {
    let document: serde_json::Map<String, serde_json::Value> = doc
        .fields
        .iter()
        .filter(|(k, _)| !exclude.contains(k))
        .map(|(k, v)| (k.clone(), value_to_json(v)))
        .collect();

    serde_json::json!({
        "_taladb_event": "insert",
        "collection": collection,
        "id": doc.id.to_string(),
        "document": serde_json::Value::Object(document),
        "timestamp": timestamp,
    })
}

/// POST `payload` to `endpoint` with retry on 5xx / network errors.
///
/// Attempts: 1 initial + 3 retries with 200 / 400 / 800 ms backoff.
/// 4xx responses are treated as permanent failures (no retry).
fn http_post_with_retry(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    headers: &HashMap<String, String>,
    payload: &serde_json::Value,
) -> Result<()> {
    const BACKOFFS_MS: [u64; 3] = [200, 400, 800];
    let max_attempts = BACKOFFS_MS.len() + 1;
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 0..max_attempts {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(BACKOFFS_MS[attempt - 1]));
        }

        let mut req = client.post(endpoint);
        for (k, v) in headers {
            req = req.header(k, v);
        }

        match req.json(payload).send() {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(resp) if resp.status().is_client_error() => {
                return Err(anyhow::anyhow!(
                    "server returned {} for {} (client error — not retrying)",
                    resp.status(),
                    endpoint
                ));
            }
            Ok(resp) => {
                last_err = Some(anyhow::anyhow!(
                    "server returned {} for {}",
                    resp.status(),
                    endpoint
                ));
            }
            Err(e) => {
                last_err = Some(anyhow::Error::new(e));
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("unknown HTTP error")))
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

pub(crate) fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Bytes(b) => serde_json::Value::String(format!("<bytes:{}>", b.len())),
        Value::Array(arr) => serde_json::Value::Array(arr.iter().map(value_to_json).collect()),
        Value::Object(obj) => serde_json::Value::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
    }
}

fn json_to_value(j: serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::Str(s),
        serde_json::Value::Array(arr) => Value::Array(arr.into_iter().map(json_to_value).collect()),
        serde_json::Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, json_to_value(v)))
                .collect(),
        ),
    }
}

fn csv_escape(v: &serde_json::Value) -> String {
    let s = match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Create an isolated temp directory containing a TalaDB file with `n`
    /// documents in `col`. Returns `(dir, db_path)` — keep `dir` alive for the
    /// duration of the test or the directory will be deleted.
    fn make_db(n: usize, col: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        for i in 0..n {
            db.collection(col)
                .unwrap()
                .insert(vec![("name".into(), Value::Str(format!("doc-{i}")))])
                .unwrap();
        }
        (dir, db_path)
    }

    /// Write `taladb.config.yml` into `dir` and return its path.
    fn write_config(dir: &std::path::Path, endpoint: &str) -> PathBuf {
        let p = dir.join("taladb.config.yml");
        std::fs::write(
            &p,
            format!("sync:\n  enabled: true\n  endpoint: \"{endpoint}\"\n"),
        )
        .unwrap();
        p
    }

    // -- payload shape (pure unit test, no HTTP) --

    #[test]
    fn sync_payload_shape() {
        use taladb_core::Document;

        let doc = Document {
            id: ulid::Ulid::new(),
            fields: vec![("name".into(), Value::Str("Alice".into()))],
        };
        let ts = 1_700_000_000_000u64;
        let payload = sync_payload("users", &doc, ts, &[]);

        assert_eq!(payload["_taladb_event"], "insert");
        assert_eq!(payload["collection"], "users");
        assert_eq!(payload["id"], doc.id.to_string());
        assert_eq!(payload["timestamp"], ts);
        assert!(payload["document"].is_object());
        assert_eq!(payload["document"]["name"], "Alice");
    }

    #[test]
    fn sync_payload_exclude_fields_stripped() {
        use taladb_core::Document;

        let doc = Document {
            id: ulid::Ulid::new(),
            fields: vec![
                ("title".into(), Value::Str("Post".into())),
                (
                    "embedding".into(),
                    Value::Array(vec![Value::Float(0.1), Value::Float(0.9)]),
                ),
            ],
        };
        let exclude = vec!["embedding".to_string()];
        let payload = sync_payload("posts", &doc, 0, &exclude);

        assert_eq!(payload["document"]["title"], "Post");
        assert!(payload["document"].get("embedding").is_none());
    }

    // -- dry-run: returns Ok and fires no HTTP requests --

    #[tokio::test]
    async fn dry_run_does_not_send_http() {
        let server = MockServer::start().await;
        // No mock registered — any unmatched request returns 404 from wiremock.
        let (dir, db_path) = make_db(3, "items");
        let cfg_path = write_config(dir.path(), &server.uri());

        tokio::task::spawn_blocking(move || {
            cmd_sync(
                &db_path,
                Some("items"),
                /*dry_run=*/ true,
                Some(&cfg_path),
            )
            .expect("cmd_sync dry-run should succeed");
        })
        .await
        .unwrap();

        assert_eq!(server.received_requests().await.unwrap().len(), 0);
    }

    // -- event count matches document count --

    #[tokio::test]
    async fn fires_one_insert_event_per_document() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/events"))
            .respond_with(ResponseTemplate::new(200))
            .expect(5)
            .mount(&server)
            .await;

        let (dir, db_path) = make_db(5, "articles");
        let endpoint = format!("{}/events", server.uri());
        let cfg_path = write_config(dir.path(), &endpoint);

        tokio::task::spawn_blocking(move || {
            cmd_sync(&db_path, Some("articles"), false, Some(&cfg_path))
                .expect("cmd_sync should succeed");
        })
        .await
        .unwrap();

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 5, "expected one request per document");
    }

    // -- payload body sent over the wire --

    #[tokio::test]
    async fn payload_body_is_correct() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let (dir, db_path) = make_db(1, "things");
        let endpoint = format!("{}/hook", server.uri());
        let cfg_path = write_config(dir.path(), &endpoint);

        tokio::task::spawn_blocking(move || {
            cmd_sync(&db_path, Some("things"), false, Some(&cfg_path))
                .expect("cmd_sync should succeed");
        })
        .await
        .unwrap();

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1, "expected exactly one request");
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["_taladb_event"], "insert");
        assert_eq!(body["collection"], "things");
        assert!(body["id"].is_string());
        assert!(body["document"].is_object());
        assert!(body["timestamp"].is_number());
        assert_eq!(body["document"]["name"], "doc-0");
    }

    // -- skipped when sync.enabled: false --

    #[tokio::test]
    async fn skipped_when_sync_disabled() {
        let server = MockServer::start().await;
        let (dir, db_path) = make_db(3, "col");

        // Config with sync explicitly disabled.
        let cfg_path = dir.path().join("taladb.config.yml");
        std::fs::write(
            &cfg_path,
            format!(
                "sync:\n  enabled: false\n  endpoint: \"{}\"\n",
                server.uri()
            ),
        )
        .unwrap();

        tokio::task::spawn_blocking(move || {
            cmd_sync(&db_path, None, false, Some(&cfg_path))
                .expect("cmd_sync should return Ok when disabled");
        })
        .await
        .unwrap();

        assert_eq!(server.received_requests().await.unwrap().len(), 0);
    }

    // -- all collections synced when no collection specified --

    #[tokio::test]
    async fn syncs_all_collections_when_none_specified() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/all"))
            .respond_with(ResponseTemplate::new(200))
            .expect(3)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        db.collection("a")
            .unwrap()
            .insert(vec![("x".into(), Value::Int(1))])
            .unwrap();
        db.collection("b")
            .unwrap()
            .insert(vec![("x".into(), Value::Int(2))])
            .unwrap();
        db.collection("b")
            .unwrap()
            .insert(vec![("x".into(), Value::Int(3))])
            .unwrap();
        drop(db);

        let endpoint = format!("{}/all", server.uri());
        let cfg_path = write_config(dir.path(), &endpoint);

        tokio::task::spawn_blocking(move || {
            cmd_sync(&db_path, None, false, Some(&cfg_path)).expect("cmd_sync should succeed");
        })
        .await
        .unwrap();

        // MockServer::Drop verifies the expect(3) — no manual assertion needed.
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 3, "1 doc in 'a' + 2 docs in 'b' = 3");
    }
}

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

use anyhow::Result;
use taladb_core::{Database, Filter, FindOptions, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::banner;
use crate::value_to_json;

const HTML: &str = include_str!("../studio.html");

// ── Entry point ────────────────────────────────────────────────────────────────

pub(crate) fn cmd_studio(file: &Path, port: u16, host: &str, no_open: bool) -> Result<()> {
    let db = crate::open_existing(&file.to_path_buf())?;
    // Loopback by default: the studio has no authentication, so any reachable
    // client can browse and delete documents.
    let addr = format!("{host}:{port}");
    let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("cannot bind to {addr}: {e}"))?;

    let url = format!("http://localhost:{port}");
    eprint!("{}", banner::studio_banner(env!("CARGO_PKG_VERSION")));
    eprint!("{}", banner::field("Database", &file.display().to_string()));
    eprint!("{}", banner::field("URL", &url));
    eprint!("{}", banner::field("Stop", "Ctrl+C"));
    eprintln!();
    if !is_loopback_host(host) {
        eprint!(
            "{}",
            banner::warn(&[
                format!("bound to {host} — the studio has no authentication;"),
                "anyone who can reach this address can read and delete data.".to_owned(),
            ])
        );
    }

    if !no_open {
        open_browser(&url);
    }

    for request in server.incoming_requests() {
        handle(request, &db, file, host);
    }

    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

// ── Browser opener ─────────────────────────────────────────────────────────────

fn open_browser(url: &str) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    let _ = result; // best-effort
}

// ── Request dispatch ───────────────────────────────────────────────────────────

fn handle(request: Request, db: &Database, file: &Path, bind_host: &str) {
    // Validate the Host header against DNS rebinding: a malicious website can
    // point its own hostname at 127.0.0.1 and issue same-origin requests to
    // this server. Such requests carry the attacker's hostname in Host, so
    // rejecting anything that isn't localhost (or the explicit bind host)
    // blocks the attack without affecting normal browser use.
    let host_ok = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| {
            let value = h.value.as_str();
            // Strip the port: "[::1]:4321" → "::1", "127.0.0.1:4321" → "127.0.0.1".
            let name = if let Some(rest) = value.strip_prefix('[') {
                rest.split(']').next().unwrap_or(rest)
            } else {
                value.rsplit_once(':').map(|(n, _)| n).unwrap_or(value)
            };
            is_loopback_host(name) || name == bind_host
        })
        .unwrap_or(false);
    if !host_ok {
        let _ = request.respond(resp_error(403, "forbidden: unexpected Host header"));
        return;
    }

    let url = request.url().to_owned();
    let method = request.method().clone();

    let path = url.split('?').next().unwrap_or("/");
    let qs = parse_qs(&url);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let resp = match (&method, segs.as_slice()) {
        // Serve the UI
        (Method::Get, []) | (Method::Get, ["index.html"]) => resp_html(HTML),

        // REST API
        (Method::Get, ["api", "collections"]) => api_collections(db, file),
        (Method::Get, ["api", "collections", name, "documents"]) => api_documents(db, name, &qs),
        (Method::Delete, ["api", "collections", name, "documents", id]) => api_delete(db, name, id),

        _ => resp_error(404, "not found"),
    };

    let _ = request.respond(resp);
}

// ── API handlers ───────────────────────────────────────────────────────────────

fn api_collections(db: &Database, file: &Path) -> Response<Cursor<Vec<u8>>> {
    let names = match db.list_collection_names() {
        Ok(n) => n,
        Err(e) => return resp_error(500, &e.to_string()),
    };

    let collections: Vec<serde_json::Value> = names
        .iter()
        .map(|name| {
            let count = db
                .collection(name)
                .and_then(|c| c.count(Filter::All))
                .unwrap_or(0);
            serde_json::json!({ "name": name, "count": count })
        })
        .collect();

    resp_json(
        200,
        &serde_json::json!({
            "db": file.display().to_string(),
            "version": env!("CARGO_PKG_VERSION"),
            "collections": collections,
        }),
    )
}

fn api_documents(
    db: &Database,
    collection: &str,
    qs: &HashMap<String, String>,
) -> Response<Cursor<Vec<u8>>> {
    let page: u64 = qs
        .get("page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let per_page: u64 = qs
        .get("per_page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
        .clamp(1, 500);

    let col = match db.collection(collection) {
        Ok(c) => c,
        Err(e) => return resp_error(400, &e.to_string()),
    };

    let total = match col.count(Filter::All) {
        Ok(n) => n,
        Err(e) => return resp_error(500, &e.to_string()),
    };

    let opts = FindOptions {
        skip: (page - 1) * per_page,
        limit: Some(per_page),
        ..Default::default()
    };

    let docs = match col.find_with_options(Filter::All, opts) {
        Ok(d) => d,
        Err(e) => return resp_error(500, &e.to_string()),
    };

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

    let pages = total.div_ceil(per_page).max(1);

    resp_json(
        200,
        &serde_json::json!({
            "documents": json_docs,
            "total": total,
            "page": page,
            "per_page": per_page,
            "pages": pages,
        }),
    )
}

fn api_delete(db: &Database, collection: &str, id: &str) -> Response<Cursor<Vec<u8>>> {
    let col = match db.collection(collection) {
        Ok(c) => c,
        Err(e) => return resp_error(400, &e.to_string()),
    };

    let filter = Filter::Eq("_id".into(), Value::Str(id.into()));
    match col.delete_one(filter) {
        Ok(true) => resp_json(200, &serde_json::json!({ "ok": true })),
        Ok(false) => resp_error(404, "document not found"),
        Err(e) => resp_error(500, &e.to_string()),
    }
}

// ── Response helpers ───────────────────────────────────────────────────────────

fn resp_html(html: &str) -> Response<Cursor<Vec<u8>>> {
    Response::from_data(html.as_bytes().to_vec())
        .with_header(content_type("text/html; charset=utf-8"))
}

fn resp_json(status: u16, body: &serde_json::Value) -> Response<Cursor<Vec<u8>>> {
    Response::from_data(serde_json::to_vec(body).unwrap_or_default())
        .with_status_code(status)
        .with_header(content_type("application/json"))
}

fn resp_error(status: u16, msg: &str) -> Response<Cursor<Vec<u8>>> {
    resp_json(status, &serde_json::json!({ "error": msg }))
}

fn content_type(ct: &str) -> Header {
    Header::from_bytes("Content-Type", ct).unwrap()
}

// ── Query-string parser ────────────────────────────────────────────────────────

fn parse_qs(url: &str) -> HashMap<String, String> {
    let qs = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    qs.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (percent_decode(k), percent_decode(v)))
        .collect()
}

/// Decode a `application/x-www-form-urlencoded` fragment: `%XX` → byte,
/// `+` → space.  Malformed `%` sequences are passed through verbatim so
/// parsing never fails — the caller gets whatever bytes the client sent.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned())
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU16, Ordering};
    use taladb_core::Value;
    use tempfile::TempDir;

    // ── Port allocation ────────────────────────────────────────────────────────
    // Start high to avoid clashing with other services or other test suites.
    static NEXT_PORT: AtomicU16 = AtomicU16::new(19_100);

    fn next_port() -> u16 {
        NEXT_PORT.fetch_add(1, Ordering::Relaxed)
    }

    // ── Test server harness ────────────────────────────────────────────────────

    /// Spins up a real `tiny_http` server on a random high port, pre-populated
    /// with the requested documents. The server runs in a background thread for
    /// the lifetime of the `TestServer` value.
    struct TestServer {
        port: u16,
        client: reqwest::blocking::Client,
        _dir: TempDir, // keeps temp dir alive until dropped
    }

    /// One seed collection: `(collection_name, documents)`.
    type SeedCollection<'a> = (&'a str, Vec<Vec<(String, Value)>>);

    impl TestServer {
        /// `cols`: slice of `(collection_name, documents)`.
        fn new(cols: &[SeedCollection]) -> Self {
            let port = next_port();
            let dir = tempfile::tempdir().unwrap();
            let db_path = dir.path().join("test.db");

            // Insert test data then drop the handle so the server thread can
            // open the file (redb holds an exclusive write lock per process).
            {
                let db = Database::open(&db_path).unwrap();
                for (name, docs) in cols {
                    let col = db.collection(name).unwrap();
                    for doc in docs.iter() {
                        col.insert(doc.clone()).unwrap();
                    }
                }
            }

            let path2 = db_path;
            std::thread::spawn(move || {
                let db = Database::open(&path2).unwrap();
                let server = Server::http(format!("127.0.0.1:{port}")).unwrap();
                for request in server.incoming_requests() {
                    handle(request, &db, &path2, "127.0.0.1");
                }
            });

            // Poll until the port accepts connections.
            //
            // The deadline is generous because it costs nothing when things are
            // healthy — the loop exits on the first successful connect — and
            // because the work behind it is not free: a thread spawn, a redb
            // open, and a socket bind, times however many of these tests the
            // runner has in flight. At 3 s this suite failed intermittently on
            // a machine that was also compiling, which is the ordinary state of
            // a developer's laptop and of CI.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "test server on port {port} did not accept connections within 30s"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }

            Self {
                port,
                client: reqwest::blocking::Client::new(),
                _dir: dir,
            }
        }

        fn get(&self, path: &str) -> reqwest::blocking::Response {
            self.client
                .get(format!("http://127.0.0.1:{}{}", self.port, path))
                .send()
                .unwrap()
        }

        fn delete(&self, path: &str) -> reqwest::blocking::Response {
            self.client
                .delete(format!("http://127.0.0.1:{}{}", self.port, path))
                .send()
                .unwrap()
        }
    }

    /// Convenience: build a `Vec<(String, Value)>` from a list of `(&str, Value)` pairs.
    fn doc(pairs: &[(&str, Value)]) -> Vec<(String, Value)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    // =========================================================================
    // Unit tests — parse_qs
    // =========================================================================

    #[test]
    fn parse_qs_no_query_string() {
        assert!(parse_qs("/path").is_empty());
    }

    #[test]
    fn parse_qs_single_param() {
        let m = parse_qs("/path?page=2");
        assert_eq!(m.get("page").map(String::as_str), Some("2"));
    }

    #[test]
    fn parse_qs_multiple_params() {
        let m = parse_qs("/foo?page=3&per_page=25");
        assert_eq!(m.get("page").map(String::as_str), Some("3"));
        assert_eq!(m.get("per_page").map(String::as_str), Some("25"));
    }

    #[test]
    fn parse_qs_empty_value() {
        let m = parse_qs("/path?key=");
        assert_eq!(m.get("key").map(String::as_str), Some(""));
    }

    #[test]
    fn parse_qs_only_parses_after_question_mark() {
        let m = parse_qs("/api/collections/users/documents?page=1");
        assert!(m.contains_key("page"));
        assert!(!m.contains_key("api"));
        assert!(!m.contains_key("users"));
    }

    #[test]
    fn parse_qs_no_value_pair_skipped() {
        // A bare key with no `=` should not appear in the map.
        let m = parse_qs("/path?standalone&key=val");
        assert!(!m.contains_key("standalone"));
        assert_eq!(m.get("key").map(String::as_str), Some("val"));
    }

    // =========================================================================
    // Integration tests — GET / (HTML)
    // =========================================================================

    #[test]
    fn root_returns_200_html() {
        let srv = TestServer::new(&[]);
        let resp = srv.get("/");
        assert_eq!(resp.status().as_u16(), 200);
        let ct = resp.headers()["content-type"].to_str().unwrap();
        assert!(ct.contains("text/html"));
    }

    #[test]
    fn root_html_contains_brand_title() {
        let srv = TestServer::new(&[]);
        let body = srv.get("/").text().unwrap();
        assert!(body.contains("TalaDB Studio"));
    }

    // =========================================================================
    // Integration tests — GET /api/collections
    // =========================================================================

    #[test]
    fn collections_empty_db_returns_empty_array() {
        let srv = TestServer::new(&[]);
        let resp = srv.get("/api/collections");
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["collections"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn collections_response_includes_db_path() {
        let srv = TestServer::new(&[]);
        let body: serde_json::Value = srv.get("/api/collections").json().unwrap();
        assert!(body["db"].as_str().unwrap().ends_with("test.db"));
    }

    #[test]
    fn collections_content_type_is_json() {
        let srv = TestServer::new(&[]);
        let resp = srv.get("/api/collections");
        let ct = resp.headers()["content-type"].to_str().unwrap();
        assert!(ct.contains("application/json"));
    }

    #[test]
    fn collections_lists_all_collection_names() {
        let srv = TestServer::new(&[
            ("users", vec![doc(&[("name", Value::Str("Alice".into()))])]),
            ("posts", vec![doc(&[("title", Value::Str("Hi".into()))])]),
        ]);
        let body: serde_json::Value = srv.get("/api/collections").json().unwrap();
        let cols = body["collections"].as_array().unwrap();
        assert_eq!(cols.len(), 2);
        let names: Vec<&str> = cols.iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"users"));
        assert!(names.contains(&"posts"));
    }

    #[test]
    fn collections_count_matches_inserted_documents() {
        let srv = TestServer::new(&[(
            "users",
            vec![
                doc(&[("name", Value::Str("Alice".into()))]),
                doc(&[("name", Value::Str("Bob".into()))]),
                doc(&[("name", Value::Str("Carol".into()))]),
            ],
        )]);
        let body: serde_json::Value = srv.get("/api/collections").json().unwrap();
        let users = body["collections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "users")
            .unwrap();
        assert_eq!(users["count"], 3);
    }

    // =========================================================================
    // Integration tests — GET /api/collections/:name/documents
    // =========================================================================

    #[test]
    fn documents_empty_collection() {
        let srv = TestServer::new(&[("empty", vec![])]);
        let body: serde_json::Value = srv.get("/api/collections/empty/documents").json().unwrap();
        assert_eq!(body["total"], 0);
        assert_eq!(body["page"], 1);
        assert_eq!(body["pages"], 1);
        assert_eq!(body["documents"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn documents_returns_inserted_fields() {
        let srv = TestServer::new(&[(
            "items",
            vec![doc(&[
                ("name", Value::Str("Widget".into())),
                ("price", Value::Int(99)),
                ("active", Value::Bool(true)),
            ])],
        )]);
        let body: serde_json::Value = srv.get("/api/collections/items/documents").json().unwrap();
        let docs = body["documents"].as_array().unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0]["name"], "Widget");
        assert_eq!(docs[0]["price"], 99);
        assert_eq!(docs[0]["active"], true);
    }

    #[test]
    fn documents_every_doc_has_id_field() {
        let srv = TestServer::new(&[(
            "things",
            vec![doc(&[("x", Value::Int(1))]), doc(&[("x", Value::Int(2))])],
        )]);
        let body: serde_json::Value = srv.get("/api/collections/things/documents").json().unwrap();
        for d in body["documents"].as_array().unwrap() {
            let id = d["_id"].as_str().unwrap();
            assert!(!id.is_empty());
        }
    }

    #[test]
    fn documents_total_pages_per_page_correct() {
        let docs: Vec<_> = (0..7i64).map(|i| doc(&[("n", Value::Int(i))])).collect();
        let srv = TestServer::new(&[("nums", docs)]);
        let body: serde_json::Value = srv
            .get("/api/collections/nums/documents?per_page=3")
            .json()
            .unwrap();
        assert_eq!(body["total"], 7);
        assert_eq!(body["per_page"], 3);
        assert_eq!(body["pages"], 3); // ceil(7/3) = 3
        assert_eq!(body["page"], 1);
        assert_eq!(body["documents"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn documents_page_two_returns_correct_slice() {
        let docs: Vec<_> = (0..10i64).map(|i| doc(&[("n", Value::Int(i))])).collect();
        let srv = TestServer::new(&[("nums", docs)]);
        let body: serde_json::Value = srv
            .get("/api/collections/nums/documents?page=2&per_page=4")
            .json()
            .unwrap();
        assert_eq!(body["page"], 2);
        assert_eq!(body["documents"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn documents_last_page_returns_remainder() {
        let docs: Vec<_> = (0..7i64).map(|i| doc(&[("n", Value::Int(i))])).collect();
        let srv = TestServer::new(&[("nums", docs)]);
        let body: serde_json::Value = srv
            .get("/api/collections/nums/documents?page=3&per_page=3")
            .json()
            .unwrap();
        // Page 3 of ceil(7/3)=3 pages: should have 1 document (7 - 2*3 = 1)
        assert_eq!(body["documents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn documents_page_beyond_range_returns_empty() {
        let docs: Vec<_> = (0..3i64).map(|i| doc(&[("n", Value::Int(i))])).collect();
        let srv = TestServer::new(&[("nums", docs)]);
        let body: serde_json::Value = srv
            .get("/api/collections/nums/documents?page=99")
            .json()
            .unwrap();
        assert_eq!(body["documents"].as_array().unwrap().len(), 0);
        assert_eq!(body["total"], 3);
    }

    #[test]
    fn documents_invalid_page_defaults_to_one() {
        let docs: Vec<_> = (0..5i64).map(|i| doc(&[("n", Value::Int(i))])).collect();
        let srv = TestServer::new(&[("nums", docs)]);
        // page=0 should clamp to 1
        let body: serde_json::Value = srv
            .get("/api/collections/nums/documents?page=0")
            .json()
            .unwrap();
        assert_eq!(body["page"], 1);
    }

    // =========================================================================
    // Integration tests — DELETE /api/collections/:name/documents/:id
    // =========================================================================

    #[test]
    fn delete_document_returns_ok_true() {
        let srv = TestServer::new(&[(
            "things",
            vec![doc(&[("name", Value::Str("to-delete".into()))])],
        )]);
        let list: serde_json::Value = srv.get("/api/collections/things/documents").json().unwrap();
        let id = list["documents"][0]["_id"].as_str().unwrap().to_owned();

        let resp = srv.delete(&format!("/api/collections/things/documents/{id}"));
        assert_eq!(resp.status().as_u16(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["ok"], true);
    }

    #[test]
    fn delete_document_removes_it_from_collection() {
        let srv = TestServer::new(&[("things", vec![doc(&[("name", Value::Str("gone".into()))])])]);
        let list: serde_json::Value = srv.get("/api/collections/things/documents").json().unwrap();
        let id = list["documents"][0]["_id"].as_str().unwrap().to_owned();

        srv.delete(&format!("/api/collections/things/documents/{id}"));

        let after: serde_json::Value = srv.get("/api/collections/things/documents").json().unwrap();
        assert_eq!(after["total"], 0);
        assert_eq!(after["documents"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn delete_nonexistent_id_returns_404() {
        let srv = TestServer::new(&[("things", vec![])]);
        let resp = srv.delete("/api/collections/things/documents/01HWZZZZZZZZZZZZZZZZZZZZZZ");
        assert_eq!(resp.status().as_u16(), 404);
        let body: serde_json::Value = resp.json().unwrap();
        assert!(body["error"].as_str().is_some());
    }

    #[test]
    fn delete_only_removes_the_targeted_document() {
        let srv = TestServer::new(&[(
            "things",
            vec![
                doc(&[("n", Value::Int(1))]),
                doc(&[("n", Value::Int(2))]),
                doc(&[("n", Value::Int(3))]),
            ],
        )]);
        let list: serde_json::Value = srv.get("/api/collections/things/documents").json().unwrap();
        let id = list["documents"][0]["_id"].as_str().unwrap().to_owned();

        srv.delete(&format!("/api/collections/things/documents/{id}"));

        let after: serde_json::Value = srv.get("/api/collections/things/documents").json().unwrap();
        assert_eq!(after["total"], 2);
        // The deleted ID must not appear in the remaining documents.
        let remaining_ids: Vec<&str> = after["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["_id"].as_str().unwrap())
            .collect();
        assert!(!remaining_ids.contains(&id.as_str()));
    }

    // =========================================================================
    // Integration tests — routing
    // =========================================================================

    #[test]
    fn rejects_foreign_host_header() {
        // Simulates DNS rebinding: the request reaches the server but carries
        // the attacker's hostname in the Host header.
        let srv = TestServer::new(&[("things", vec![doc(&[("n", Value::Int(1))])])]);
        let resp = srv
            .client
            .get(format!("http://127.0.0.1:{}/api/collections", srv.port))
            .header("Host", "evil.example.com")
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403);
    }

    #[test]
    fn unknown_route_returns_404() {
        let srv = TestServer::new(&[]);
        let resp = srv.get("/api/does-not-exist");
        assert_eq!(resp.status().as_u16(), 404);
    }

    #[test]
    fn unknown_route_response_has_error_field() {
        let srv = TestServer::new(&[]);
        let body: serde_json::Value = srv.get("/api/does-not-exist").json().unwrap();
        assert!(body["error"].as_str().is_some());
    }

    #[test]
    fn index_html_path_also_serves_ui() {
        let srv = TestServer::new(&[]);
        let resp = srv.get("/index.html");
        assert_eq!(resp.status().as_u16(), 200);
        let ct = resp.headers()["content-type"].to_str().unwrap();
        assert!(ct.contains("text/html"));
    }

    #[test]
    fn collections_count_updates_after_delete() {
        let srv = TestServer::new(&[(
            "log",
            vec![
                doc(&[("msg", Value::Str("a".into()))]),
                doc(&[("msg", Value::Str("b".into()))]),
            ],
        )]);

        let before: serde_json::Value = srv.get("/api/collections").json().unwrap();
        let count_before = before["collections"][0]["count"].as_u64().unwrap();
        assert_eq!(count_before, 2);

        let list: serde_json::Value = srv.get("/api/collections/log/documents").json().unwrap();
        let id = list["documents"][0]["_id"].as_str().unwrap().to_owned();
        srv.delete(&format!("/api/collections/log/documents/{id}"));

        let after: serde_json::Value = srv.get("/api/collections").json().unwrap();
        let count_after = after["collections"][0]["count"].as_u64().unwrap();
        assert_eq!(count_after, 1);
    }
}

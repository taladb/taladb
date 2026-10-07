---
title: Rust Guide
description: Use TalaDB as the local database of a Rust application — serde documents, JSON filters, indexes, vector, full-text and hybrid search, live queries and encryption, in one embedded file.
---

# Rust

Use TalaDB as the local database of a Rust application: a desktop app, a CLI,
a service, an edge worker. It is the engine every other TalaDB platform runs,
used directly — documents, filters, indexes,
[vector search](/api/vector-search), [full-text search](/api/search), live
queries and encryption at rest, in one file, inside your process.

The `taladb` crate is published to [crates.io](https://crates.io/crates/taladb)
with each TalaDB release. The full API reference is on [docs.rs](https://docs.rs/taladb).

## Installation

```sh
cargo add taladb serde --features serde/derive
cargo add serde_json
```

To try unreleased changes, depend on the repository instead:

```toml
taladb = { git = "https://github.com/taladb/taladb" }
```

Requires Rust 1.90 or newer.

## Quick start

```rust
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize, Deserialize)]
struct Note {
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    title: String,
    done: bool,
}

fn main() -> Result<(), taladb::TalaDbError> {
    let db = taladb::open("app.db")?;              // creates the file if needed
    let notes = db.typed::<Note>("notes")?;

    notes.create_index("done")?;                   // idempotent: fine at every start
    notes.insert(&Note { id: None, title: "Buy groceries".into(), done: false })?;

    let open: Vec<Note> = notes.find(json!({ "done": false }))?;
    notes.update_one(json!({ "title": "Buy groceries" }), json!({ "$set": { "done": true } }))?;
    notes.delete_many(json!({ "done": true }))?;
    Ok(())
}
```

- **Your types** only need `serde`. Every stored document has an `_id`;
  declare it as an optional field renamed to `_id`, as above, to read it back.
  Leave it `None` on insert and the engine assigns a ULID.
- **Filters and updates** are JSON, in the same language as every TalaDB
  platform — see [Filters](/api/filters) and [Updates](/api/updates). A
  malformed filter is an error, never a silent match-all.
- **Errors** are one type, `TalaDbError`.

## Threads and async

`Database` is cheap to clone, and every clone shares the same open database —
hand one to each thread. Operations are synchronous and fast; in async code,
run them with `tokio::task::spawn_blocking` (or your runtime's equivalent) so
they do not stall the executor.

## Vector, full-text and hybrid search

```rust
#[derive(Serialize, Deserialize)]
struct Doc { title: String, embedding: Vec<f32> }

let docs = db.typed::<Doc>("docs")?;
docs.create_vector_index("embedding", 384, None, None)?;  // exact search by default
docs.create_fts_index("title")?;

let similar = docs.find_nearest("embedding", &query_embedding, 5, None)?;
let matches = docs.search_text("title", "groceries", 5)?;
```

Exact vector search is faster and always exact below tens of thousands of
vectors; pass `HnswOptions` for a persistent approximate graph beyond that.
`find_nearest` takes an optional filter — `Some(json!({ "kind": "note" }))` —
that narrows the candidates before ranking.

### Hybrid search

Hybrid search fuses BM25 text relevance with vector similarity, for queries
where exact terms and meaning both matter — retrieval for on-device RAG, for
example. It lives on the underlying collection, reached with `.raw()`:

```rust
use taladb::fts::HybridQuery;
use taladb::json::{document_to_json, filter_from_json};

let hits = docs.raw().hybrid_search(
    HybridQuery::new("title", "groceries", "embedding", &query_embedding, 5)
        .filter(filter_from_json(&json!({ "done": false }))?),
)?;
for hit in &hits {
    let doc: Doc = serde_json::from_value(document_to_json(&hit.document))?;
    // hit.score is the fused rank score; hit.text_rank / hit.vector_rank are
    // zero-based positions in each ranking, or None if absent from one.
}
```

## Sorting and pagination

`find` returns every match. To sort, page or project, use the underlying
collection's `find_with_options`, and decode the results into your type:

```rust
use taladb::json::{document_to_json, filter_from_json};
use taladb::{FindOptions, SortSpec};

let page = notes.raw().find_with_options(
    filter_from_json(&json!({ "done": false }))?,
    FindOptions {
        sort: vec![SortSpec::desc("stars")],
        skip: 0,
        limit: Some(20),
        ..Default::default()
    },
)?;
let page: Vec<Note> = page
    .iter()
    .map(|doc| serde_json::from_value(document_to_json(doc)))
    .collect::<Result<_, _>>()?;
```

`FindOptions` also takes `fields` (return only these fields, plus `_id`) and
`timeout` (fail a query that runs too long).

## Aggregation

Pipelines are JSON too — `$match`, `$group`, `$sort`, `$skip`, `$limit`,
`$project` ([reference](/api/aggregation)):

```rust
use taladb::json::{document_to_json, filter_from_json};

let pipeline = taladb::aggregate::parse_pipeline(
    &json!([
        { "$group": { "_id": "$done", "total": { "$sum": "$stars" } } },
        { "$sort": { "_id": 1 } },
    ]),
    &|f| filter_from_json(f).map_err(|e| e.to_string()),
)
.map_err(taladb::TalaDbError::InvalidOperation)?;

for row in notes.raw().aggregate(pipeline)? {
    println!("{}", document_to_json(&row));   // {"_id":false,"total":4}
}
```

## Live queries

```rust
let watch = notes.watch(json!({ "done": false }))?;
let current = notes.find(json!({ "done": false }))?;   // read *after* subscribing

std::thread::spawn(move || {
    while let Ok(open) = watch.next() {                  // blocks until a write changes it
        render(&open);
    }
});
```

Writes through any clone of the database wake the watch. Rapid writes coalesce
into one snapshot of the latest state, and none is skipped. `next_timeout`
waits with a deadline, so a loop can stop cleanly. See
[Live Queries](/api/live-queries).

## Encryption

```rust
let db = taladb::Database::open_encrypted(std::path::Path::new("secret.db"), &passphrase)?;
```

Enable the `encryption` feature: `cargo add taladb --features encryption`. See
[Encryption](/api/encryption).

## Migrations

Storage-format upgrades run automatically at open. For your own schema steps,
`db.user_version()` and `db.set_user_version(n)` record which have run — the
same counter the other platforms' [migration runners](/api/migrations) use:

```rust
let db = taladb::open("app.db")?;
if db.user_version()? < 1 {
    db.typed::<Note>("notes")?.create_index("done")?;
    db.set_user_version(1)?;
}
if db.user_version()? < 2 {
    db.typed::<Note>("notes")?.update_many(
        json!({ "stars": { "$exists": false } }),
        json!({ "$set": { "stars": 0 } }),
    )?;
    db.set_user_version(2)?;
}
```

Bump the version after each step succeeds, so a step that fails runs again on
the next start — and write each step so running it twice is harmless.

## The untyped API

Under every typed collection is a `Collection` of dynamically typed documents:
fields are `(String, Value)` pairs and filters are the `Filter` enum. Reach it
with `db.collection(name)` or `typed.raw()` — both see the same documents.

```rust
use taladb::{Filter, Value};

let events = db.collection("events")?;
events.insert(vec![
    ("kind".into(), Value::Str("login".into())),
    ("ms".into(), Value::Int(42)),
])?;
let slow = events.find(Filter::Gt("ms".into(), Value::Int(10)))?;
let ms = slow[0].get("ms");   // Some(&Value::Int(42))
```

`taladb::json` converts between the two: `filter_from_json`,
`update_from_json`, `fields_from_json` and `document_to_json`.

## Errors

Every operation returns `Result<_, TalaDbError>`. Match the variants you can
handle:

```rust
use taladb::TalaDbError;

match notes.find(json!({ "stars": { "$gtt": 1 } })) {
    Err(TalaDbError::InvalidFilter(filter)) => eprintln!("bad filter: {filter}"),
    Err(e) => return Err(e.into()),
    Ok(found) => { /* … */ }
}
```

Common ones: `InvalidFilter` (malformed filter), `InvalidOperation` (malformed
update or pipeline), `IndexNotFound`, `DuplicateId`, `VectorDimensionMismatch`,
`Serialization` (a document did not match your type), and `Encryption` (wrong
passphrase).

## API at a glance

**`TypedCollection<T>`** — from `db.typed::<T>(name)`:

| Method | Returns |
|---|---|
| `insert(&T)` / `insert_many(&[T])` | the new id(s) — `insert_many` is all or nothing |
| `find(filter)` / `find_one(filter)` / `find_by_id(id)` | `Vec<T>` / `Option<T>` |
| `count(filter)` | `u64` |
| `update_one(filter, update)` / `update_many(filter, update)` | whether one matched / how many changed |
| `delete_one(filter)` / `delete_many(filter)` | whether one matched / how many were deleted |
| `create_index` · `create_compound_index` · `create_fts_index` · `create_vector_index` | `()` — all idempotent |
| `find_nearest(field, &[f32], top_k, filter)` | `Vec<Scored<T>>`, best first |
| `search_text(field, query, top_k)` | `Vec<Scored<T>>`, best first |
| `watch(filter)` | `TypedWatch<T>` — `next()`, `next_timeout(d)`, `try_next()` |
| `raw()` | the underlying `Collection` |

**`Collection`** adds `find_with_options`, `aggregate`, `hybrid_search`,
`search_text_with` (BM25 tuning), `drop_*` for every index kind,
`list_indexes`, and HNSW maintenance.

**`Database`** — `taladb::open(path)`, `open_in_memory()`,
`open_encrypted(path, passphrase)`, `typed::<T>(name)`, `collection(name)`,
`list_collection_names()`, `flush()`, `compact()`, `export_snapshot()` /
`restore_from_snapshot()`, `user_version()` / `set_user_version()`.

Full signatures and every type are on [docs.rs/taladb](https://docs.rs/taladb).

## Features

| Feature | Default | What it adds |
|---|:---:|---|
| `config-yaml` | ✅ | Reads `taladb.config.yml`. |
| `legacy-migration` | ✅ | Opens files written by TalaDB before 0.11. Turn it off to drop a second copy of redb from the build. |
| `encryption` | | AES-GCM encryption at rest. |

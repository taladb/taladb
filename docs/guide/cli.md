---
title: CLI Dev Tools
description: Inspect, export, import, and manage TalaDB database files from the terminal using the taladb CLI. Includes vector index inspection and embedding-aware export/import workflows.
---

# CLI Dev Tools

The `taladb` CLI lets you inspect, export, and manage TalaDB database files from the terminal — useful for debugging, data migration, and CI pipelines.

## Installation

Download the pre-built binary for your platform from the [GitHub Releases page](https://github.com/taladb/taladb/releases).

::: code-group

```sh [Linux]
# Download and extract (replace VERSION with the latest release, e.g. v0.1.1)
curl -L https://github.com/taladb/taladb/releases/download/VERSION/taladb-linux-x86_64-VERSION.tar.gz \
  | tar -xz
sudo mv taladb /usr/local/bin/
taladb --version
```

```sh [macOS]
curl -L https://github.com/taladb/taladb/releases/download/VERSION/taladb-macos-aarch64-VERSION.tar.gz \
  | tar -xz
sudo mv taladb /usr/local/bin/
taladb --version
```

```powershell [Windows]
# Download taladb-windows-x86_64-VERSION.zip from the releases page,
# extract it, and add the folder to your PATH.
taladb --version
```

:::

## Commands

### `studio` — local web UI

Start a local browser-based UI for exploring and editing a TalaDB database. The server runs until you press Ctrl+C.

```sh
taladb studio ./myapp.db
```

A browser window opens automatically at `http://localhost:4321`. The UI shows all collections in the sidebar, displays documents in a paginated table, lets you filter with the search bar, inspect the full JSON of any document, and delete individual documents.

```sh
# Use a different port
taladb studio ./myapp.db --port 8080

# Print the URL but do not open a browser
taladb studio ./myapp.db --no-open
```

**Flags**

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `4321` | Port to listen on |
| `--no-open` | `false` | Skip opening a browser window |

The database file is opened read-write. Deletions made through the UI are permanent and cannot be undone.

**Platform compatibility**

`taladb studio` opens any `.db` file on disk using the Rust core directly — it is not specific to any one SDK.

| Platform | Works? | Notes |
|---|---|---|
| **Node.js** (`@taladb/node`) | ✅ Yes | Primary use case — point at the same `.db` file your app uses |
| **React Native** | ✅ Yes | Copy the `.db` off the device first (see below) |
| **Browser / OPFS** | ✗ No | OPFS files live inside the browser's sandboxed filesystem, unreachable from the CLI |
| **Cloudflare Workers** | ✗ No | State lives in Durable Objects storage, not a local file |

For **React Native**, pull the file with `adb` (Android) or Xcode's device file browser (iOS), then open it:

```sh
adb pull /data/data/com.myapp/files/myapp.db ./myapp.db
taladb studio ./myapp.db
```

For **browser / OPFS**, export a snapshot from within your app and download it, then open the downloaded file:

```ts
// In your app — export to a downloadable file
const bytes = await db.exportSnapshot()
const a = Object.assign(document.createElement('a'), {
  href: URL.createObjectURL(new Blob([bytes])),
  download: 'myapp.db',
})
a.click()
```

```sh
taladb studio ~/Downloads/myapp.db
```

---

### `inspect` — database overview

Print all collections, their document counts, and any vector indexes defined on them.

```sh
taladb inspect ./myapp.db
```

```
TalaDB Inspector
────────────────
File: myapp.db

Collections (3):
  articles  (1 247 documents)
    Indexes:     category, locale, publishedAt
    Vector indexes:  embedding (384-dim, cosine)
  sessions  (8 documents)
  users     (56 documents)
    Indexes:     email, age
```

Vector indexes are shown under the collection they belong to, with their configured dimensions and similarity metric.

---

### `collections` — list collection names

Print one collection name per line (useful for scripting).

```sh
taladb collections ./myapp.db
```

---

### `count` — count documents

```sh
taladb count ./myapp.db users
# 56
```

---

### `export` — dump a collection

Export all documents in a collection to JSON, NDJSON, or CSV.

```sh
# Pretty-printed JSON array (default) — prints to stdout
taladb export ./myapp.db users

# Write to a file
taladb export ./myapp.db users --out users.json

# Newline-delimited JSON (one document per line)
taladb export ./myapp.db users --fmt ndjson --out users.ndjson

# CSV (flat fields only)
taladb export ./myapp.db users --fmt csv --out users.csv
```

**Flags**

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--fmt` | `-f` | `json` | Output format: `json`, `ndjson`, `csv` |
| `--out` | `-o` | stdout | Output file path |

**Embedding fields** are exported as regular JSON arrays of numbers — no special handling needed. A document with an `embedding` field exports exactly as stored:

```json
{
  "_id": "01HWZZQ0000000000000000000",
  "title": "How to reset your password",
  "embedding": [0.023, -0.141, 0.887, "...383 more values..."]
}
```

This means export + import round-trips preserve embedding data faithfully. The **vector index itself is not exported** — only the raw field values are. After importing into a new database, call `createVectorIndex` in your application startup to rebuild the index from the stored embedding fields.

---

### `import` — bulk insert from JSON / NDJSON

Import documents from a JSON array file or NDJSON file. New ULIDs are assigned — any `_id` fields in the source are ignored.

```sh
# From a JSON array
taladb import ./myapp.db users users.json

# From NDJSON
taladb import ./myapp.db users users.ndjson
```

The database file is created if it does not exist.

**Importing documents with embeddings:** If your exported documents contain numeric array fields (e.g. `embedding`), those values are inserted as-is. The vector index is **not** automatically created — call `createVectorIndex` in your application after import to make the field searchable:

```ts
// After taladb import ./dev.db articles articles.ndjson
const db = await openDB('./dev.db')
await db.collection('articles').createVectorIndex('embedding', { dimensions: 384 })
// Backfill runs automatically — all imported docs with a valid 'embedding' field are indexed
```

---

### `drop` — clear a collection

Delete all documents in a collection. Indexes defined on the collection are preserved.

```sh
taladb drop ./myapp.db sessions
# Deleted 8 documents from 'sessions'
```

---

### `upgrade-vector-index` — rebuild HNSW graph

Rebuild the in-memory HNSW graph for a vector index from the current flat vector table. Use this after bulk imports, or whenever the HNSW index has grown stale due to writes since the graph was last built.

```sh
taladb upgrade-vector-index ./myapp.db articles embedding
# HNSW graph for 'articles::embedding' rebuilt successfully.
```

| Argument | Description |
|---|---|
| `<file>` | Path to the TalaDB database file |
| `<collection>` | Collection name |
| `<field>` | Vector field name |

This command is a no-op when:
- The index was created as flat-only (no HNSW options were stored)
- The binary was compiled without the `vector-hnsw` feature

You can also trigger this programmatically: see [`upgradeVectorIndex`](/api/collection#upgradevectorindexfield) in the Collection API docs.

---

## Common workflows

**Back up a collection before a migration:**

```sh
taladb export ./prod.db users --out users-backup.json
```

**Seed a local dev database from production export:**

```sh
taladb import ./dev.db users users-backup.json
```

**Check document counts in CI:**

```sh
COUNT=$(taladb count ./test.db results)
if [ "$COUNT" -lt 1 ]; then
  echo "No results written — test failed"
  exit 1
fi
```

**Seed a vector collection from an exported dataset:**

Export from one database, import into another, then rebuild the vector index at app startup:

```sh
# 1. Export from production (embeddings included as plain JSON arrays)
taladb export ./prod.db articles --fmt ndjson --out articles.ndjson

# 2. Import into local dev database
taladb import ./dev.db articles articles.ndjson

# 3. In your app startup — createVectorIndex backfills all imported docs automatically
```

```ts
const db = await openDB('./dev.db')
const articles = db.collection('articles')
await articles.createVectorIndex('embedding', { dimensions: 384 })
```

**Verify vector data is present after import:**

```sh
# Count should match what was exported
taladb count ./dev.db articles

# Inspect to confirm the vector index was created by the app
taladb inspect ./dev.db
# articles  (1247 documents)
#   Vector indexes:  embedding (384-dim, cosine)
```

---

### `sync` — push entire database {#sync-push-entire-database}

Push all documents in the database (or a single collection) to the HTTP endpoint configured in `taladb.config.yml`. Each document is sent as an `insert` event — same payload shape as the real-time push sync hook.

Requires `sync.enabled: true` in the config. See the [HTTP Push Sync guide](/guide/http-sync) for full setup instructions.

```sh
# Push all collections to the configured endpoint
taladb sync ./myapp.db

# Push a single collection
taladb sync ./myapp.db articles

# Preview events without sending (prints JSON to stdout)
taladb sync ./myapp.db articles --dry-run

# Use an explicit config file instead of auto-discovery
taladb sync ./myapp.db --config ./config/taladb.prod.yml
```

**Flags**

| Flag | Description |
|------|-------------|
| `--dry-run` | Print each event as pretty-printed JSON without sending any HTTP requests |
| `--config <path>` | Explicit path to a config file. Auto-discovers `taladb.config.yml` from the database file's directory when omitted |

**Progress output** (stderr):

```
Syncing articles... 142/142 ✓
Syncing users... 56/56 ✓
Done. 198 event(s) sent.
```

**Notes:**
- `exclude_fields` from the config is respected — embedding vectors and other large fields are stripped from payloads if configured.
- HTTP failures on individual documents are propagated as errors — the command stops at the first failure. Re-run to resume.
- If `sync.enabled: false` (or no config file is found), the command prints a message and exits cleanly without sending anything.

---

## Planned commands

The following commands are planned for a future release:

| Command | Description |
|---|---|
| `taladb vector-indexes ./myapp.db` | List all vector indexes across all collections |
| `taladb find-nearest ./myapp.db <collection> <field> <vector-json> --top 5` | Run a similarity query from the terminal |
| `taladb drop-vector-index ./myapp.db <collection> <field>` | Remove a vector index |

Track progress on the [GitHub issues page](https://github.com/taladb/taladb/issues).

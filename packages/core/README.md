# taladb-core

**The Rust engine behind [TalaDB](https://github.com/taladb/taladb) — an embedded
vector database for on-device AI.**

Documents, structured queries, and vector similarity search in one embedded
database that runs where the model runs: the browser (WASM), React Native, and
Node.js. This crate is the engine itself; most applications consume it through a
language binding rather than directly.

- **Docs:** <https://taladb.dev>
- **Repository:** <https://github.com/taladb/taladb>
- **JavaScript packages:** [`taladb`](https://www.npmjs.com/package/taladb),
  [`@taladb/web`](https://www.npmjs.com/package/@taladb/web),
  [`@taladb/react`](https://www.npmjs.com/package/@taladb/react)

## Using it from Rust

```rust
use taladb_core::{Database, Filter, Value};

# fn main() -> Result<(), taladb_core::TalaDbError> {
let db = Database::open_in_memory()?;
let books = db.collection("books")?;

books.insert(vec![
    ("title".into(), Value::Str("Noli Me Tángere".into())),
    ("year".into(), Value::Int(1887)),
])?;

let found = books.find(Filter::Gte("year".into(), Value::Int(1800)))?;
assert_eq!(found.len(), 1);
# Ok(())
# }
```

Vector search works over the same documents:

```rust
# use taladb_core::{Database, Value};
# fn main() -> Result<(), taladb_core::TalaDbError> {
# let db = Database::open_in_memory()?;
let notes = db.collection("notes")?;
notes.create_vector_index("embedding", 384, None, None)?;

let hits = notes.find_nearest("embedding", &vec![0.0; 384], 10, None)?;
# let _ = hits;
# Ok(())
# }
```

## Features

| Feature | Default | What it adds |
|---|:---:|---|
| `config-yaml` | ✅ | Reads `taladb.config.yml`. Browser and React Native builds turn this off (`default-features = false`) to drop `serde_yaml` and `unsafe-libyaml` from the bundle. |
| `encryption` | | AES-GCM encrypted storage with PBKDF2 key derivation. |
| `vector-hnsw` | | HNSW approximate nearest-neighbour index. Without it, vector search is exact (brute force) — correct, and fast enough well past 100k vectors. |
| `sync-http` | | HTTP push replication to an origin. |

## Runtime targets

The browser and React Native are the primary targets; native and Node are
supported but are not what the design optimises for. Anything that would bloat a
WASM bundle is feature-gated.

## Benchmarks

```
cargo bench -p taladb-core
```

Covers vector scoring, query filtering, and insert throughput. See
[`benches/`](benches/).

## License

Apache-2.0

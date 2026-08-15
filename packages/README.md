# Packages

Grouped by role. The dependency direction is one-way: **core** ← **bindings** ← **clients / adapters / integrations**.

| Folder | Role | Contents |
|---|---|---|
| `core/` | **The engine** | `taladb-core` — pure Rust: document model, indexes, vector search, query planner, storage. No JS bindings. |
| `bindings/` | **Runtime wrappers over core** | `node/` (napi → `@taladb/node`), `web/` (wasm → `@taladb/web`), `react-native/` (JSI → `@taladb/react-native`). Rust + JS hybrids. |
| `clients/` | **What apps import** | `taladb/` (unified meta-package → `taladb`), `react/` (→ `@taladb/react`). Pure TypeScript. |
| `adapters/` | **Sync adapters** | `mongodb/` (→ `@taladb/sync-mongodb`). Pure TypeScript. Implement the `SyncAdapter` interface from `taladb`. |
| `tools/` | **Dev tooling** | `cli/` (`taladb-cli` Rust binary). |

## Conventions

- **Folder location ≠ npm/crate name.** npm names live in each `package.json` (`@taladb/*`, or unscoped `taladb`); crate names live in `Cargo.toml`. Moving a folder never changes a published name.
- **A new sync adapter** goes in `adapters/<backend>/` as `@taladb/sync-<backend>`, implementing `SyncAdapter` (`push`/`pull`) from `taladb`. See `adapters/mongodb` as the template. (The zero-dependency `HttpSyncAdapter` ships inside the `taladb` client package as the batteries-included default.)
- **A new runtime binding** goes in `bindings/<runtime>/` and wraps `core` — it must not contain engine logic.
- **Cross-package deps use names, not paths** (`"taladb": "workspace:*"`), so they survive folder moves.

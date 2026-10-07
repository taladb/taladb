---
title: iOS & macOS (Swift) Guide
description: Use TalaDB in native iOS and macOS apps from Swift — documents, vector search, full-text and hybrid search, live queries as async streams, and migrations, without React Native.
---

# iOS & macOS (Swift)

Use TalaDB from a native iOS or macOS app written in Swift, without React
Native. It is the same Rust engine as every other runtime: documents, filters,
indexes, [vector search](/api/vector-search),
[full-text and hybrid search](/api/search), live queries and encryption at
rest, all on the device.

Feedback and issues are welcome on
[taladb/taladb-swift](https://github.com/taladb/taladb-swift).

## Requirements

- iOS 13+ or macOS 10.15+
- Swift 5.9+ (Xcode 15+), through Swift Package Manager

## Installation

Add the package:

```swift
dependencies: [
    .package(url: "https://github.com/taladb/taladb-swift", from: "0.1.1"),
],
targets: [
    .target(name: "MyApp", dependencies: [.product(name: "TalaDB", package: "taladb-swift")]),
]
```

### Build from source

SwiftPM downloads the prebuilt engine (`TalaDBFFI.xcframework`) and verifies
its checksum, so this step is only for working on the package itself or trying
unreleased engine changes. On a Mac with Rust installed, build the engine's
xcframework from a checkout of this repository, then use the package locally:

```sh
git clone https://github.com/taladb/taladb
git clone https://github.com/taladb/taladb-swift
cd taladb-swift
scripts/build-engine.sh ../taladb     # builds engine/TalaDBFFI.xcframework
swift test
```

Add it to your app with **File → Add Package Dependencies → Add Local…**, or
`.package(path: "../taladb-swift")`.

## Quick start

```swift
import TalaDB

struct Note: Codable, Sendable {
    var id: String?
    var title: String
    var tags: [String] = []
    enum CodingKeys: String, CodingKey { case id = "_id", title, tags }
}

let url = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    .appendingPathComponent("app.db")
let db = try await TalaDB.open(at: url)          // keep one per process
let notes = db.collection("notes", as: Note.self)

let id = try await notes.insert(Note(title: "Groceries", tags: ["home"]))
let home = try await notes.find(["tags": "home"])
try await notes.updateOne(["_id": .string(id)], ["$set": ["title": "Weekly groceries"]])
```

Every operation is `async` and runs off the calling thread, so it is safe to
call from the main actor. One `TalaDB` can be shared across tasks.

- **Filters, updates and pipelines** are `JSONValue` literals using the same
  operators as every other runtime — see [Filters](/api/filters),
  [Updates](/api/updates) and [Aggregation](/api/aggregation).
- **`_id`**: leave it nil on insert and the engine assigns a ULID.
- **Untyped access**: `db.collection("name")` works with raw `JSONValue`s.
- **Errors**: engine errors throw `TalaDBError.engine`; calls after `close()`
  throw `.closed`.

## Live queries

`watch` returns an `AsyncThrowingStream`. It yields the current result, then a
fresh one after every write that changes it. Rapid writes coalesce into one
element, and nothing is missed:

```swift
for try await open in notes.watch(["done": false]) {
    render(open)
}
```

See [Live Queries](/api/live-queries).

## Vector and hybrid search

```swift
try await notes.createVectorIndex("embedding", dimensions: 384)
let similar = try await notes.findNearest("embedding", vector: queryVector, topK: 5)

try await notes.createFtsIndex("title")
let hits = try await notes.hybridSearch(textField: "title", text: "groceries",
                                        vectorField: "embedding", vector: queryVector, topK: 5)
```

`searchVectors` adds exact or approximate mode, `efSearch`, thresholds,
pagination and grouping. `rebuildVectorIndex` builds an HNSW graph in
batches with progress reporting, and is cancelled with its task.

## Migrations

```swift
let db = try await TalaDB.open(at: url, migrations: [
    Migration(1, "Index users by email") { db in
        try await db.collection("users").createIndex("email")
    },
])
```

Pending migrations run in version order at open, with the same
checkpoint-per-version semantics as [Migrations](/api/migrations).

## Encryption

```swift
let db = try await TalaDB.open(at: url, config: TalaDBConfig(passphrase: key))
```

See [Encryption](/api/encryption).

## How it works

Swift calls the engine's C FFI directly — the prebuilt `TalaDBFFI.xcframework`
carries the header and a module map, so no glue code is needed. At open the
package checks that the library's C ABI version matches the one it was built
for, so a mismatched engine fails with a clear error. Full API reference:
[taladb/taladb-swift](https://github.com/taladb/taladb-swift).

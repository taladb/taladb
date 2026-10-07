---
title: Android (Kotlin) Guide
description: Use TalaDB in native Android apps from Kotlin — documents, vector search, full-text and hybrid search, live queries as Flows, and migrations, without React Native.
---

# Android (Kotlin)

Use TalaDB from a native Android app written in Kotlin, without React Native.
It is the same Rust engine as every other runtime: documents, filters,
indexes, [vector search](/api/vector-search),
[full-text and hybrid search](/api/search), live queries and encryption at
rest, all on the device.

To see it in a real app, install [Notewell](https://play.google.com/store/apps/details?id=dev.thinkgrid.notewell) from Google Play, a
native Kotlin notebook built on this package. Feedback and issues are welcome
on [taladb/taladb-kotlin](https://github.com/taladb/taladb-kotlin).

## Requirements

- `minSdk` 24 or higher
- Kotlin 2.2+, with the kotlinx.serialization plugin for typed collections
- ABIs: `arm64-v8a`, `armeabi-v7a`, `x86_64`, all 16 KB page aligned as Google
  Play requires for apps targeting Android 15+

## Installation

Add the dependency and the serialization plugin:

```kotlin
plugins {
    id("org.jetbrains.kotlin.plugin.serialization") version "<your Kotlin version>"
}

dependencies {
    implementation("dev.taladb:taladb-android:0.1.1")
}
```

### Build from source

To work on the package itself or try unreleased engine changes, build the AAR
from the repository. You need JDK 17, the Android
SDK with an NDK, Rust with `cargo-ndk`, and a checkout of this engine next to
it:

```sh
git clone https://github.com/taladb/taladb
git clone https://github.com/taladb/taladb-kotlin
cd taladb-kotlin
scripts/build-engine.sh ../taladb                       # builds the engine for every ABI
./gradlew :taladb:publishToMavenLocal                    # dev.taladb:taladb-android:<version>
```

Then add `mavenLocal()` to your app's repositories and depend on the version
in the repository's `gradle.properties` (`VERSION_NAME`).

## Quick start

```kotlin
@Serializable
data class Note(
    @SerialName("_id") val id: String? = null,
    val title: String,
    val tags: List<String> = emptyList(),
)

val db = TalaDB.open(context.filesDir.resolve("app.db"))   // keep one per process
val notes = db.collection<Note>("notes")

val id = notes.insert(Note(title = "Groceries", tags = listOf("home")))
val home = notes.find(buildJsonObject { put("tags", "home") })

notes.updateOne(
    buildJsonObject { put("_id", id) },
    buildJsonObject { putJsonObject("\$set") { put("title", "Weekly groceries") } },
)
```

Every operation is a `suspend` function that runs on `Dispatchers.IO` (or a
dispatcher you pass to `open`), so it is safe to call from the main thread.
One `TalaDB` can be shared across threads and coroutines.

- **Filters, updates and pipelines** use the same operators as every other
  runtime — see [Filters](/api/filters), [Updates](/api/updates) and
  [Aggregation](/api/aggregation) — built with `buildJsonObject`.
- **`_id`**: leave it `null` on insert and the engine assigns a ULID.
- **Untyped access**: `db.collection("name")` works with raw `JsonObject`s.
- **Errors**: engine errors throw `TalaDBException`; calls after `close()`
  throw `IllegalStateException`.

## Live queries

`watch` returns a `Flow`. It emits the current result, then a fresh one after
every write that changes it. Rapid writes coalesce into one emission, and
nothing is missed:

```kotlin
notes.watch(buildJsonObject { put("done", false) })
    .collect { open -> render(open) }
```

See [Live Queries](/api/live-queries).

## Vector and hybrid search

```kotlin
notes.createVectorIndex("embedding", dimensions = 384)
val similar = notes.findNearest("embedding", queryVector, topK = 5)

notes.createFtsIndex("title")
val hits = notes.hybridSearch("title", "groceries", "embedding", queryVector, topK = 5)
```

`searchVectors` adds exact or approximate mode, `efSearch`, thresholds,
pagination and grouping. `rebuildVectorIndex` builds an HNSW graph in
cancellable batches with progress reporting.

## Migrations

```kotlin
val db = TalaDB.open(file, migrations = listOf(
    Migration(1, "Index users by email") { db -> db.collection("users").createIndex("email") },
))
```

Pending migrations run in version order at open, with the same
checkpoint-per-version semantics as [Migrations](/api/migrations).

## Encryption

```kotlin
val db = TalaDB.open(file, TalaDBConfig(passphrase = key))
```

See [Encryption](/api/encryption).

## How it works

The package calls the engine's C FFI through a small JNI layer, and ships the
engine's prebuilt Android libraries in the AAR. At open it checks that the
library's C ABI version matches the one it was built for, so a mismatched
engine fails with a clear error. Full API reference:
[taladb/taladb-kotlin](https://github.com/taladb/taladb-kotlin).

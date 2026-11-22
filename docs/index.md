---
layout: home
markdownStyles: false
title: TalaDB — One database. Everywhere your app runs.
titleTemplate: false
description: TalaDB is an embedded document and vector database with a Rust core. Store documents, run structured queries, and use vector, full-text and hybrid search in the browser, Node.js, React Native, Android and iOS. Local-first, no server required.
---

<!--
  Code on this page is real Markdown, so VitePress highlights it with Shiki
  and adds copy buttons. Each snippet mirrors the Quick start of its platform
  guide; change both together.
-->

<HomeHero>

```ts
import { openDB } from 'taladb'

const db = await openDB('app.db')
const notes = db.collection('notes')

// Documents and their embeddings live together
await notes.createVectorIndex('embedding', {
  dimensions: 384,
})
await notes.insert({
  title: 'Trip ideas',
  archived: false,
  embedding: await embed('Trip ideas'),
})

// Filter on metadata first, then rank by similarity
const similar = await notes.findNearest(
  'embedding',
  await embed('weekend getaway'),
  5,
  { archived: false },
)
```

</HomeHero>

<HomeFeatures />

<HomeArchitecture />

<HomeQuickStart>
<template #web>

<p class="qs-step">1. Install</p>

```sh
pnpm add taladb @taladb/web
```

<p class="qs-step">2. Configure Vite: OPFS needs a cross-origin isolated worker</p>

```ts
// vite.config.ts
import { defineConfig } from 'vite'

export default defineConfig({
  optimizeDeps: { exclude: ['taladb', '@taladb/web'] },
  server: {
    headers: {
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'credentialless',
    },
  },
})
```

<p class="qs-step">3. Open a database and query</p>

```ts
import { openDB } from 'taladb'

const db = await openDB('myapp.db') // persisted to OPFS
const users = db.collection<{ name: string; age: number }>('users')

await users.insert({ name: 'Alice', age: 30 })
const adults = await users.find({ age: { $gte: 18 } })
```

</template>
<template #node>

<p class="qs-step">1. Install</p>

```sh
pnpm add taladb @taladb/node
```

<p class="qs-step">2. Open a database and query</p>

```ts
import { openDB } from 'taladb'

const db = await openDB('./data.db') // parent directory must exist
const products = db.collection<{ name: string; price: number }>('products')

await products.createIndex('price')
await products.insert({ name: 'Widget', price: 9.99 })
const cheap = await products.find({ price: { $lte: 20 } })
```

</template>
<template #rn>

<p class="qs-step">1. Install (New Architecture, React Native 0.73+)</p>

```sh
pnpm add taladb @taladb/react-native
cd ios && pod install
```

<p class="qs-step">2. Initialize once at startup, then use the same API as the web</p>

```ts
import { TalaDBModule } from '@taladb/react-native'
import { openDB } from 'taladb'

await TalaDBModule.initialize('myapp.db')

const db = await openDB('myapp.db')
const notes = db.collection<{ body: string; pinned: boolean }>('notes')

await notes.insert({ body: 'Buy milk', pinned: false })
const pinned = await notes.find({ pinned: true })
```

</template>
<template #android>

<p class="qs-step">1. Add the dependency (Maven Central, minSdk 24)</p>

```kotlin
// build.gradle.kts — typed collections also need the kotlinx.serialization plugin
dependencies {
    implementation("dev.taladb:taladb-android:0.2.3")
}
```

<p class="qs-step">2. Open a database and query: every call is a suspend function</p>

```kotlin
@Serializable
data class Note(
    @SerialName("_id") val id: String? = null,
    val title: String,
    val tags: List<String> = emptyList(),
)

val db = TalaDB.open(context.filesDir.resolve("app.db"))
val notes = db.collection<Note>("notes")

notes.insert(Note(title = "Groceries", tags = listOf("home")))
val home = notes.find(buildJsonObject { put("tags", "home") })
```

</template>
<template #swift>

<p class="qs-step">1. Add the package (iOS 13+, macOS 10.15+)</p>

```swift
.package(url: "https://github.com/taladb/taladb-swift", from: "0.2.3")
// target dependency: .product(name: "TalaDB", package: "taladb-swift")
```

<p class="qs-step">2. Open a database and query: every call is async</p>

```swift
import TalaDB

struct Note: Codable, Sendable {
    var id: String?
    var title: String
    var tags: [String] = []
    enum CodingKeys: String, CodingKey { case id = "_id", title, tags }
}

let db = try await TalaDB.open(at: url)
let notes = db.collection("notes", as: Note.self)

try await notes.insert(Note(title: "Groceries", tags: ["home"]))
let home = try await notes.find(["tags": "home"])
```

</template>
<template #rust>

<p class="qs-step">1. Add the crate (Rust 1.90+)</p>

```sh
cargo add taladb serde --features serde/derive
cargo add serde_json
```

<p class="qs-step">2. Open a database and query</p>

```rust
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize, Deserialize)]
struct Note {
    title: String,
    done: bool,
}

fn main() -> Result<(), taladb::TalaDbError> {
    let db = taladb::open("app.db")?;
    let notes = db.typed::<Note>("notes")?;

    notes.insert(&Note { title: "Buy groceries".into(), done: false })?;
    let open: Vec<Note> = notes.find(json!({ "done": false }))?;
    println!("{} open notes", open.len());
    Ok(())
}
```

</template>
</HomeQuickStart>

<HomeCTA />

---
title: React Native Guide
description: Use TalaDB in React Native apps for on-device vector search, BM25 and hybrid retrieval, and document storage — no server required.
---

# React Native

Ship on-device AI on mobile: store embeddings from a local model and run [vector search](/api/vector-search), [BM25 full-text](/api/search), and [hybrid RAG retrieval](/api/search#hybrid-search) directly on the phone — private by default, and available offline.

TalaDB runs natively on iOS and Android via a JSI integration — calls from JavaScript go directly into the Rust engine without bridge overhead or JSON serialisation on the hot path. Search calls are synchronous via JSI.


## Requirements

- React Native **0.73+** with New Architecture enabled
- Expo SDK **50+** (if using Expo)
- Xcode 15+ for iOS builds
- Android NDK **r27+** for Android builds

## Installation

```bash
pnpm add taladb @taladb/react-native
```

### iOS

```bash
cd ios && pod install
```

### Android

No extra steps — Gradle links the native library automatically.

### Enable the New Architecture

TalaDB requires the New Architecture for its JSI integration.

**`android/gradle.properties`**
```properties
newArchEnabled=true
```

**`ios/Podfile`**
```ruby
use_framework! :static
```

## Quick start

Call `TalaDBModule.initialize` once at app startup — before any component tries to use the database.

```ts
// App.tsx
import { TalaDBModule } from '@taladb/react-native'
import { openDB } from 'taladb'

await TalaDBModule.initialize('myapp.db')

const db = await openDB('myapp.db')
const users = db.collection('users')

await users.insert({ name: 'Alice', createdAt: Date.now() })
const all = await users.find()
```

That's it. The `taladb` package detects React Native automatically — the same code you write for the browser or Node.js works here too.

## Change webhook

Report every committed write to a backend over HTTP. Configure it on `openDB` —
not on `TalaDBModule.initialize`, which now carries only storage settings
(durability, encryption passphrase):

```ts
import { TalaDBModule } from '@taladb/react-native'
import { openDB } from 'taladb'

await TalaDBModule.initialize('myapp.db')

const db = await openDB('myapp.db', {
  webhook: {
    enabled: true,
    endpoint: 'https://api.example.com/taladb',
    headers: { Authorization: `Bearer ${myToken}` },
    exclude_fields: ['embedding'],  // omit large vector fields
  },
})
// Every write now fires an HTTP request after the commit
```

`POST` on insert, `PUT` on update, `DELETE` on delete. Delivery happens after the
commit on a bounded in-memory queue with 3 retries and exponential backoff
(200 / 400 / 800 ms) on 5xx and network errors. The write path is never blocked;
a saturated queue drops events rather than applying back-pressure.

Observe delivery health and drain before logout or backgrounding:

```ts
db.webhookStats?.()          // { pending, delivered, failed, dropped }
await db.flushWebhook?.(5_000)
await db.close()             // drains automatically
```

The webhook is a best-effort event stream with **at-most-once** delivery, not a
durable replication queue. If events must survive process termination or a long
offline period, your app needs its own outbox or a reconciliation pass.

Per-op endpoint overrides are supported:

```ts
const db = await openDB('myapp.db', {
  webhook: {
    enabled: true,
    endpoint: 'https://api.example.com/events',
    insert_endpoint: 'https://api.example.com/events/insert',
    update_endpoint: 'https://api.example.com/events/update',
    delete_endpoint: 'https://api.example.com/events/delete',
  },
})
```

Delivery runs in the `taladb` TypeScript client on `fetch`, so it behaves exactly
as it does on the browser and Node.js. See the
[Change Webhook reference](/api/webhook).

## Full example

```tsx
// App.tsx
import React, { useEffect, useState } from 'react'
import { View, Text, Button, FlatList } from 'react-native'
import { TalaDBModule } from '@taladb/react-native'
import { openDB, type Collection } from 'taladb'

interface Note {
  _id?: string
  text: string
  createdAt: number
}

let notes: Collection<Note>

export default function App() {
  const [items, setItems] = useState<Note[]>([])

  useEffect(() => {
    async function init() {
      await TalaDBModule.initialize('notes.db')
      const db = await openDB('notes.db')
      notes = db.collection<Note>('notes')
      await notes.createIndex('createdAt')
      setItems(await notes.find())
    }
    init()
  }, [])

  async function addNote() {
    await notes.insert({ text: `Note ${Date.now()}`, createdAt: Date.now() })
    setItems(await notes.find())
  }

  return (
    <View style={{ flex: 1, padding: 40 }}>
      <Button title="Add Note" onPress={addNote} />
      <FlatList
        data={items}
        keyExtractor={(item) => item._id!}
        renderItem={({ item }) => <Text>{item.text}</Text>}
      />
    </View>
  )
}
```

## Vector search

TalaDB supports on-device semantic search — store embeddings from a local ML model (Core ML, TensorFlow Lite) and search them without any server.

```ts
interface Article {
  _id?: string
  title: string
  body: string
  embedding: number[]
}

const articles = db.collection<Article>('articles')
await articles.createVectorIndex('embedding', { dimensions: 384 })

// Insert with embedding from your on-device model
const embedding = await myModel.embed(content)
await articles.insert({ title, body: content, embedding })

// Semantic search
const queryVec = await myModel.embed(userQuery)
const results = await articles.findNearest('embedding', queryVec, 5)

results.forEach(({ document, score }) => {
  console.log(score.toFixed(3), document.title)
})

// Filtered vector search: metadata filter applied before ranking
const filtered = await articles.findNearest('embedding', queryVec, 5, {
  category: 'faq',
})
```

## Where data is stored

| Platform | Location |
|---|---|
| iOS | `NSDocumentDirectory` (iCloud-excluded by default) |
| Android | `Context.getFilesDir()` (app-private, no permissions needed) |

No special permissions are required on either platform.

## Migrations

```ts
const db = await openDB('myapp.db', {
  migrations: [
    {
      version: 1,
      description: 'Add notes index',
      up: async (db) => {
        await db.collection('notes').createIndex('createdAt')
      },
    },
  ],
})
```

## Troubleshooting

**`__TalaDB__ JSI HostObject not found`**
`openDB` was called before `TalaDBModule.initialize` completed. Move `initialize` to the very top of your app entry point and `await` it before any database access.

**`TurboModuleRegistry.getEnforcing('TalaDB'): 'TalaDB' could not be found`** (Android)
The native module was not linked. Verify that your `android/gradle.properties` has `newArchEnabled=true` and that you are using a **custom dev client** — `@taladb/react-native` cannot run inside Expo Go.

**`New Architecture is not enabled`**
Set `newArchEnabled=true` in `android/gradle.properties` and add `use_framework! :static` to your `ios/Podfile`.

**Pod install fails on iOS**
Make sure Xcode command-line tools are active: `xcode-select --install`. Then re-run `pod install`.

## Current limitations

- **Expo Go** — not supported. You must use a custom dev client (`expo prebuild`).
- **Live queries (`subscribe`)** — polling-based on React Native; native file-watch push is planned for a future release.

# @taladb/react-native

React Native module for TalaDB — embedded local-first storage via JSI TurboModule and a Rust core.

[![npm](https://img.shields.io/npm/v/@taladb/react-native)](https://www.npmjs.com/package/@taladb/react-native)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-orange.svg)](LICENSE)

> **Note:** Most users should install [`taladb`](https://www.npmjs.com/package/taladb) instead, which auto-selects this package when running in React Native.

## What it provides

- TalaDB's Rust core compiled to a static library for iOS and Android
- JSI HostObject bridge — calls go directly from JS to C++ to Rust with no JSON serialization on the hot path
- No Bridge/async overhead for local reads
- Data stored in the app's private documents directory (sandboxed, backed up by OS)

## Requirements

- React Native 0.73+
- iOS 15+ / Android API 24+

## Installation

```bash
pnpm add taladb @taladb/react-native
```

### iOS

```bash
cd ios && pod install
```

### Android

No extra setup required — the `.so` library is bundled automatically via Gradle.

## Usage

Use through the unified [`taladb`](https://www.npmjs.com/package/taladb) package:

```ts
import { openDB } from 'taladb';

const db = await openDB('myapp.db');
const tasks = db.collection('tasks');

await tasks.createIndex('status');
await tasks.insert({ title: 'Buy groceries', status: 'pending', priority: 1 });

const pending = await tasks.find({ status: 'pending' });
await tasks.updateOne({ title: 'Buy groceries' }, { $set: { status: 'done' } });
```

### With React hooks

```tsx
import { useEffect, useState } from 'react';
import { openDB } from 'taladb';
import type { TalaDB } from 'taladb';

export function useDatabase() {
  const [db, setDb] = useState<TalaDB | null>(null);

  useEffect(() => {
    let instance: TalaDB;
    openDB('myapp.db').then((opened) => {
      instance = opened;
      setDb(opened);
    });
    return () => { instance?.close(); };
  }, []);

  return db;
}
```

## Architecture

```
JavaScript (React Native)
        │  JSI (synchronous, no bridge)
        ▼
C++ HostObject (TalaDBHostObject.cpp)
        │  C FFI
        ▼
Rust static library (taladb-core)
        │
        ▼
redb B-tree (app documents directory)
```

## Building from source

```bash
# iOS (requires Xcode + Rust iOS targets)
pnpm --filter @taladb/react-native build:ios

# Android (requires NDK + Rust Android targets)
pnpm --filter @taladb/react-native build:android
```

## Full Documentation

**[https://taladb.github.io/taladb/guide/react-native](https://taladb.github.io/taladb/guide/react-native)**

## License

Apache 2.0 © [TalaDB](https://github.com/taladb)

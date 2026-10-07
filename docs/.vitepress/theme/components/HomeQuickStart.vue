<template>
  <section class="home-quickstart">
    <div class="section-container">
      <div class="section-header">
        <h2 class="section-title">Up and running in minutes</h2>
        <p class="section-sub">Pick your platform and go.</p>
      </div>

      <!-- Platform tabs -->
      <div class="qs-tabs" role="tablist">
        <button
          v-for="tab in tabs"
          :key="tab.id"
          :class="['qs-tab', { active: activeTab === tab.id }]"
          role="tab"
          :aria-selected="activeTab === tab.id"
          @click="activeTab = tab.id"
        >
          {{ tab.label }}
        </button>
      </div>

      <!-- Steps -->
      <div class="qs-steps">
        <div
          v-for="(step, i) in currentTab.steps"
          :key="step.title"
          class="qs-step"
        >
          <div class="qs-step-num">{{ i + 1 }}</div>
          <div class="qs-step-body">
            <h3 class="qs-step-title">{{ step.title }}</h3>
            <p v-if="step.desc" class="qs-step-desc">{{ step.desc }}</p>
            <div class="qs-code-block">
              <div class="qs-code-header">
                <span class="qs-code-lang">{{ step.lang }}</span>
                <button class="qs-copy-btn" @click="copyCode(step.raw, i)" :aria-label="`Copy step ${i + 1} code`">
                  <svg v-if="copiedIdx !== i" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                  <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="20 6 9 17 4 12"/></svg>
                </button>
              </div>
              <pre class="qs-code"><code v-html="step.code"></code></pre>
            </div>
          </div>
        </div>
      </div>

      <div class="qs-footer">
        <a href="/guide/web" class="qs-guide-link" v-if="activeTab === 'browser'">Full Browser Guide →</a>
        <a href="/guide/node" class="qs-guide-link" v-if="activeTab === 'node'">Full Node.js Guide →</a>
        <a href="/guide/react-native" class="qs-guide-link" v-if="activeTab === 'rn'">Full React Native Guide →</a>
        <a href="/guide/android" class="qs-guide-link" v-if="activeTab === 'kotlin'">Full Android (Kotlin) Guide →</a>
        <a href="/guide/swift" class="qs-guide-link" v-if="activeTab === 'swift'">Full iOS &amp; macOS (Swift) Guide →</a>
        <a href="/guide/rust" class="qs-guide-link" v-if="activeTab === 'rust'">Full Rust Guide →</a>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref, computed } from 'vue'

const activeTab = ref('browser')
const copiedIdx = ref<number | null>(null)

function copyCode(raw: string, idx: number) {
  navigator.clipboard.writeText(raw).then(() => {
    copiedIdx.value = idx
    setTimeout(() => { copiedIdx.value = null }, 2000)
  })
}

const tabs = [
  { id: 'browser', label: 'Browser / Vite' },
  { id: 'node',    label: 'Node.js' },
  { id: 'rn',      label: 'React Native' },
  { id: 'kotlin',  label: 'Android (Kotlin)' },
  { id: 'swift',   label: 'iOS (Swift)' },
  { id: 'rust',    label: 'Rust' },
]

const allTabs: Record<string, { steps: Array<{ title: string; desc?: string; lang: string; code: string; raw: string }> }> = {
  browser: {
    steps: [
      {
        title: 'Install',
        lang: 'bash',
        raw: 'pnpm add taladb @taladb/web',
        code: `<span class="token-punctuation">$</span> pnpm add taladb @taladb/web`,
      },
      {
        title: 'Configure Vite',
        desc: 'TalaDB\'s WASM worker needs cross-origin isolation headers to use OPFS.',
        lang: 'vite.config.ts',
        raw: `import { defineConfig } from 'vite'

export default defineConfig({
  server: {
    headers: {
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
    },
  },
})`,
        code: `<span class="token-keyword">import</span> <span class="token-punctuation">{</span> defineConfig <span class="token-punctuation">}</span> <span class="token-keyword">from</span> <span class="token-string">'vite'</span>

<span class="token-keyword">export default</span> <span class="token-function">defineConfig</span><span class="token-punctuation">({</span>
  server<span class="token-punctuation">:</span> <span class="token-punctuation">{</span>
    headers<span class="token-punctuation">:</span> <span class="token-punctuation">{</span>
      <span class="token-string">'Cross-Origin-Opener-Policy'</span><span class="token-punctuation">:</span> <span class="token-string">'same-origin'</span><span class="token-punctuation">,</span>
      <span class="token-string">'Cross-Origin-Embedder-Policy'</span><span class="token-punctuation">:</span> <span class="token-string">'require-corp'</span><span class="token-punctuation">,</span>
    <span class="token-punctuation">}</span><span class="token-punctuation">,</span>
  <span class="token-punctuation">}</span><span class="token-punctuation">,</span>
<span class="token-punctuation">})</span>`,
      },
      {
        title: 'Open a database and query',
        lang: 'app.ts',
        raw: `import { openDB } from 'taladb'

const db = await openDB('myapp.db')
const users = db.collection<{ name: string; age: number }>('users')

await users.insert({ name: 'Alice', age: 30 })
const results = await users.find({ age: { $gte: 18 } })
console.log(results) // [{ _id: '...', name: 'Alice', age: 30 }]`,
        code: `<span class="token-keyword">import</span> <span class="token-punctuation">{</span> openDB <span class="token-punctuation">}</span> <span class="token-keyword">from</span> <span class="token-string">'taladb'</span>

<span class="token-keyword">const</span> db <span class="token-operator">=</span> <span class="token-keyword">await</span> <span class="token-function">openDB</span><span class="token-punctuation">(</span><span class="token-string">'myapp.db'</span><span class="token-punctuation">)</span>
<span class="token-keyword">const</span> users <span class="token-operator">=</span> db<span class="token-punctuation">.</span><span class="token-function">collection</span><span class="token-punctuation">&lt;</span><span class="token-punctuation">{</span> name<span class="token-punctuation">:</span> string<span class="token-punctuation">;</span> age<span class="token-punctuation">:</span> number <span class="token-punctuation">}&gt;(</span><span class="token-string">'users'</span><span class="token-punctuation">)</span>

<span class="token-keyword">await</span> users<span class="token-punctuation">.</span><span class="token-function">insert</span><span class="token-punctuation">(</span><span class="token-punctuation">{</span> name<span class="token-punctuation">:</span> <span class="token-string">'Alice'</span><span class="token-punctuation">,</span> age<span class="token-punctuation">:</span> <span class="token-number">30</span> <span class="token-punctuation">})</span>
<span class="token-keyword">const</span> results <span class="token-operator">=</span> <span class="token-keyword">await</span> users<span class="token-punctuation">.</span><span class="token-function">find</span><span class="token-punctuation">(</span><span class="token-punctuation">{</span> age<span class="token-punctuation">:</span> <span class="token-punctuation">{</span> <span class="token-keyword">$gte</span><span class="token-punctuation">:</span> <span class="token-number">18</span> <span class="token-punctuation">}</span> <span class="token-punctuation">})</span>
<span class="token-comment">// [{ _id: '...', name: 'Alice', age: 30 }]</span>`,
      },
    ],
  },

  node: {
    steps: [
      {
        title: 'Install',
        lang: 'bash',
        raw: 'pnpm add taladb @taladb/node',
        code: `<span class="token-punctuation">$</span> pnpm add taladb @taladb/node`,
      },
      {
        title: 'Open a database and query',
        lang: 'script.ts',
        raw: `import { openDB } from 'taladb'

const db = await openDB('data.db')
const products = db.collection<{ name: string; price: number }>('products')

await products.insert({ name: 'Widget', price: 9.99 })
await products.createIndex('price')

const cheap = await products.find({ price: { $lte: 20 } })
console.log(cheap)`,
        code: `<span class="token-keyword">import</span> <span class="token-punctuation">{</span> openDB <span class="token-punctuation">}</span> <span class="token-keyword">from</span> <span class="token-string">'taladb'</span>

<span class="token-keyword">const</span> db <span class="token-operator">=</span> <span class="token-keyword">await</span> <span class="token-function">openDB</span><span class="token-punctuation">(</span><span class="token-string">'data.db'</span><span class="token-punctuation">)</span>
<span class="token-keyword">const</span> products <span class="token-operator">=</span> db<span class="token-punctuation">.</span><span class="token-function">collection</span><span class="token-punctuation">&lt;</span><span class="token-punctuation">{</span> name<span class="token-punctuation">:</span> string<span class="token-punctuation">;</span> price<span class="token-punctuation">:</span> number <span class="token-punctuation">}&gt;(</span><span class="token-string">'products'</span><span class="token-punctuation">)</span>

<span class="token-keyword">await</span> products<span class="token-punctuation">.</span><span class="token-function">insert</span><span class="token-punctuation">(</span><span class="token-punctuation">{</span> name<span class="token-punctuation">:</span> <span class="token-string">'Widget'</span><span class="token-punctuation">,</span> price<span class="token-punctuation">:</span> <span class="token-number">9.99</span> <span class="token-punctuation">})</span>
<span class="token-keyword">await</span> products<span class="token-punctuation">.</span><span class="token-function">createIndex</span><span class="token-punctuation">(</span><span class="token-string">'price'</span><span class="token-punctuation">)</span>

<span class="token-keyword">const</span> cheap <span class="token-operator">=</span> <span class="token-keyword">await</span> products<span class="token-punctuation">.</span><span class="token-function">find</span><span class="token-punctuation">(</span><span class="token-punctuation">{</span> price<span class="token-punctuation">:</span> <span class="token-punctuation">{</span> <span class="token-keyword">$lte</span><span class="token-punctuation">:</span> <span class="token-number">20</span> <span class="token-punctuation">}</span> <span class="token-punctuation">})</span>
console<span class="token-punctuation">.</span><span class="token-function">log</span><span class="token-punctuation">(</span>cheap<span class="token-punctuation">)</span>`,
      },
    ],
  },

  rn: {
    steps: [
      {
        title: 'Install',
        lang: 'bash',
        raw: 'pnpm add taladb @taladb/react-native',
        code: `<span class="token-punctuation">$</span> pnpm add taladb @taladb/react-native`,
      },
      {
        title: 'Link native module',
        lang: 'bash',
        raw: 'npx expo prebuild\nnpx expo run:android  # or run:ios',
        code: `<span class="token-punctuation">$</span> npx expo prebuild
<span class="token-punctuation">$</span> npx expo run:android  <span class="token-comment"># or run:ios</span>`,
      },
      {
        title: 'Use exactly like web — same API',
        lang: 'App.tsx',
        raw: `import { openDB } from 'taladb'

// JSI — synchronous, no bridge latency
const db = await openDB('mobile.db')
const notes = db.collection<{ body: string; pinned: boolean }>('notes')

await notes.insert({ body: 'Buy milk', pinned: false })
const pinned = await notes.find({ pinned: true })`,
        code: `<span class="token-keyword">import</span> <span class="token-punctuation">{</span> openDB <span class="token-punctuation">}</span> <span class="token-keyword">from</span> <span class="token-string">'taladb'</span>

<span class="token-comment">// JSI — synchronous, no bridge latency</span>
<span class="token-keyword">const</span> db <span class="token-operator">=</span> <span class="token-keyword">await</span> <span class="token-function">openDB</span><span class="token-punctuation">(</span><span class="token-string">'mobile.db'</span><span class="token-punctuation">)</span>
<span class="token-keyword">const</span> notes <span class="token-operator">=</span> db<span class="token-punctuation">.</span><span class="token-function">collection</span><span class="token-punctuation">&lt;</span><span class="token-punctuation">{</span> body<span class="token-punctuation">:</span> string<span class="token-punctuation">;</span> pinned<span class="token-punctuation">:</span> boolean <span class="token-punctuation">}&gt;(</span><span class="token-string">'notes'</span><span class="token-punctuation">)</span>

<span class="token-keyword">await</span> notes<span class="token-punctuation">.</span><span class="token-function">insert</span><span class="token-punctuation">(</span><span class="token-punctuation">{</span> body<span class="token-punctuation">:</span> <span class="token-string">'Buy milk'</span><span class="token-punctuation">,</span> pinned<span class="token-punctuation">:</span> <span class="token-keyword">false</span> <span class="token-punctuation">})</span>
<span class="token-keyword">const</span> pinned <span class="token-operator">=</span> <span class="token-keyword">await</span> notes<span class="token-punctuation">.</span><span class="token-function">find</span><span class="token-punctuation">(</span><span class="token-punctuation">{</span> pinned<span class="token-punctuation">:</span> <span class="token-keyword">true</span> <span class="token-punctuation">})</span>`,
      },
    ],
  },
  kotlin: {
    steps: [
      {
        title: 'Add the dependency',
        desc: 'Published on Maven Central. Typed collections also need the kotlinx.serialization plugin.',
        lang: 'build.gradle.kts',
        raw: `dependencies {
    implementation("dev.taladb:taladb-android:0.1.1")
}`,
        code: `dependencies <span class="token-punctuation">{</span>
    <span class="token-function">implementation</span><span class="token-punctuation">(</span><span class="token-string">"dev.taladb:taladb-android:0.1.1"</span><span class="token-punctuation">)</span>
<span class="token-punctuation">}</span>`,
      },
      {
        title: 'Open a database and query',
        desc: 'Every call is a suspend function that runs off the main thread.',
        lang: 'Notes.kt',
        raw: `@Serializable
data class Note(
    @SerialName("_id") val id: String? = null,
    val title: String,
    val pinned: Boolean = false,
)

val db = TalaDB.open(context.filesDir.resolve("app.db"))
val notes = db.collection<Note>("notes")

notes.insert(Note(title = "Buy milk"))
val pinned = notes.find(buildJsonObject { put("pinned", true) })`,
        code: `<span class="token-keyword">@Serializable</span>
<span class="token-keyword">data</span> <span class="token-keyword">class</span> Note<span class="token-punctuation">(</span>
    <span class="token-keyword">@SerialName</span><span class="token-punctuation">(</span><span class="token-string">"_id"</span><span class="token-punctuation">)</span> <span class="token-keyword">val</span> id<span class="token-punctuation">:</span> String<span class="token-punctuation">?</span> <span class="token-operator">=</span> <span class="token-keyword">null</span><span class="token-punctuation">,</span>
    <span class="token-keyword">val</span> title<span class="token-punctuation">:</span> String<span class="token-punctuation">,</span>
    <span class="token-keyword">val</span> pinned<span class="token-punctuation">:</span> Boolean <span class="token-operator">=</span> <span class="token-keyword">false</span><span class="token-punctuation">,</span>
<span class="token-punctuation">)</span>

<span class="token-keyword">val</span> db <span class="token-operator">=</span> TalaDB<span class="token-punctuation">.</span><span class="token-function">open</span><span class="token-punctuation">(</span>context<span class="token-punctuation">.</span>filesDir<span class="token-punctuation">.</span><span class="token-function">resolve</span><span class="token-punctuation">(</span><span class="token-string">"app.db"</span><span class="token-punctuation">)</span><span class="token-punctuation">)</span>
<span class="token-keyword">val</span> notes <span class="token-operator">=</span> db<span class="token-punctuation">.</span><span class="token-function">collection</span><span class="token-punctuation">&lt;</span>Note<span class="token-punctuation">&gt;</span><span class="token-punctuation">(</span><span class="token-string">"notes"</span><span class="token-punctuation">)</span>

notes<span class="token-punctuation">.</span><span class="token-function">insert</span><span class="token-punctuation">(</span>Note<span class="token-punctuation">(</span>title <span class="token-operator">=</span> <span class="token-string">"Buy milk"</span><span class="token-punctuation">)</span><span class="token-punctuation">)</span>
<span class="token-keyword">val</span> pinned <span class="token-operator">=</span> notes<span class="token-punctuation">.</span><span class="token-function">find</span><span class="token-punctuation">(</span>buildJsonObject <span class="token-punctuation">{</span> <span class="token-function">put</span><span class="token-punctuation">(</span><span class="token-string">"pinned"</span><span class="token-punctuation">,</span> <span class="token-keyword">true</span><span class="token-punctuation">)</span> <span class="token-punctuation">}</span><span class="token-punctuation">)</span>`,
      },
    ],
  },

  swift: {
    steps: [
      {
        title: 'Add the package',
        desc: 'iOS 13+ and macOS 10.15+. SwiftPM downloads the prebuilt engine and verifies its checksum.',
        lang: 'Package.swift',
        raw: `.package(url: "https://github.com/taladb/taladb-swift", from: "0.1.1")
// target dependency: .product(name: "TalaDB", package: "taladb-swift")`,
        code: `<span class="token-punctuation">.</span><span class="token-function">package</span><span class="token-punctuation">(</span>url<span class="token-punctuation">:</span> <span class="token-string">"https://github.com/taladb/taladb-swift"</span><span class="token-punctuation">,</span> from<span class="token-punctuation">:</span> <span class="token-string">"0.1.1"</span><span class="token-punctuation">)</span>
<span class="token-comment">// target dependency: .product(name: "TalaDB", package: "taladb-swift")</span>`,
      },
      {
        title: 'Open a database and query',
        desc: 'Every call is async and runs off the calling thread.',
        lang: 'Notes.swift',
        raw: `import TalaDB

struct Note: Codable, Sendable {
    var id: String?
    var title: String
    var pinned = false
    enum CodingKeys: String, CodingKey { case id = "_id", title, pinned }
}

let db = try await TalaDB.open(at: url)
let notes = db.collection("notes", as: Note.self)

try await notes.insert(Note(title: "Buy milk"))
let pinned = try await notes.find(["pinned": true])`,
        code: `<span class="token-keyword">import</span> TalaDB

<span class="token-keyword">struct</span> Note<span class="token-punctuation">:</span> Codable<span class="token-punctuation">,</span> Sendable <span class="token-punctuation">{</span>
    <span class="token-keyword">var</span> id<span class="token-punctuation">:</span> String<span class="token-punctuation">?</span>
    <span class="token-keyword">var</span> title<span class="token-punctuation">:</span> String
    <span class="token-keyword">var</span> pinned <span class="token-operator">=</span> <span class="token-keyword">false</span>
    <span class="token-keyword">enum</span> CodingKeys<span class="token-punctuation">:</span> String<span class="token-punctuation">,</span> CodingKey <span class="token-punctuation">{</span> <span class="token-keyword">case</span> id <span class="token-operator">=</span> <span class="token-string">"_id"</span><span class="token-punctuation">,</span> title<span class="token-punctuation">,</span> pinned <span class="token-punctuation">}</span>
<span class="token-punctuation">}</span>

<span class="token-keyword">let</span> db <span class="token-operator">=</span> <span class="token-keyword">try</span> <span class="token-keyword">await</span> TalaDB<span class="token-punctuation">.</span><span class="token-function">open</span><span class="token-punctuation">(</span>at<span class="token-punctuation">:</span> url<span class="token-punctuation">)</span>
<span class="token-keyword">let</span> notes <span class="token-operator">=</span> db<span class="token-punctuation">.</span><span class="token-function">collection</span><span class="token-punctuation">(</span><span class="token-string">"notes"</span><span class="token-punctuation">,</span> as<span class="token-punctuation">:</span> Note<span class="token-punctuation">.</span>self<span class="token-punctuation">)</span>

<span class="token-keyword">try</span> <span class="token-keyword">await</span> notes<span class="token-punctuation">.</span><span class="token-function">insert</span><span class="token-punctuation">(</span>Note<span class="token-punctuation">(</span>title<span class="token-punctuation">:</span> <span class="token-string">"Buy milk"</span><span class="token-punctuation">)</span><span class="token-punctuation">)</span>
<span class="token-keyword">let</span> pinned <span class="token-operator">=</span> <span class="token-keyword">try</span> <span class="token-keyword">await</span> notes<span class="token-punctuation">.</span><span class="token-function">find</span><span class="token-punctuation">(</span><span class="token-punctuation">[</span><span class="token-string">"pinned"</span><span class="token-punctuation">:</span> <span class="token-keyword">true</span><span class="token-punctuation">]</span><span class="token-punctuation">)</span>`,
      },
    ],
  },

  rust: {
    steps: [
      {
        title: 'Install',
        lang: 'bash',
        raw: `cargo add taladb serde --features serde/derive
cargo add serde_json`,
        code: `<span class="token-punctuation">$</span> cargo add taladb serde --features serde/derive
<span class="token-punctuation">$</span> cargo add serde_json`,
      },
      {
        title: 'Open a database and query',
        lang: 'main.rs',
        raw: `use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize, Deserialize)]
struct Note {
    title: String,
    pinned: bool,
}

fn main() -> Result<(), taladb::TalaDbError> {
    let db = taladb::open("app.db")?;
    let notes = db.typed::<Note>("notes")?;

    notes.insert(&Note { title: "Buy milk".into(), pinned: false })?;
    let pinned: Vec<Note> = notes.find(json!({ "pinned": true }))?;
    Ok(())
}`,
        code: `<span class="token-keyword">use</span> serde<span class="token-punctuation">:</span><span class="token-punctuation">:</span><span class="token-punctuation">{</span>Deserialize<span class="token-punctuation">,</span> Serialize<span class="token-punctuation">}</span><span class="token-punctuation">;</span>
<span class="token-keyword">use</span> serde_json<span class="token-punctuation">:</span><span class="token-punctuation">:</span>json<span class="token-punctuation">;</span>

<span class="token-keyword">#[derive</span><span class="token-punctuation">(</span>Serialize<span class="token-punctuation">,</span> Deserialize<span class="token-punctuation">)</span><span class="token-punctuation">]</span>
<span class="token-keyword">struct</span> Note <span class="token-punctuation">{</span>
    title<span class="token-punctuation">:</span> String<span class="token-punctuation">,</span>
    pinned<span class="token-punctuation">:</span> bool<span class="token-punctuation">,</span>
<span class="token-punctuation">}</span>

<span class="token-keyword">fn</span> <span class="token-function">main</span><span class="token-punctuation">(</span><span class="token-punctuation">)</span> -<span class="token-punctuation">&gt;</span> Result<span class="token-punctuation">&lt;</span><span class="token-punctuation">(</span><span class="token-punctuation">)</span><span class="token-punctuation">,</span> taladb<span class="token-punctuation">:</span><span class="token-punctuation">:</span>TalaDbError<span class="token-punctuation">&gt;</span> <span class="token-punctuation">{</span>
    <span class="token-keyword">let</span> db <span class="token-operator">=</span> taladb<span class="token-punctuation">:</span><span class="token-punctuation">:</span><span class="token-function">open</span><span class="token-punctuation">(</span><span class="token-string">"app.db"</span><span class="token-punctuation">)</span><span class="token-punctuation">?</span><span class="token-punctuation">;</span>
    <span class="token-keyword">let</span> notes <span class="token-operator">=</span> db<span class="token-punctuation">.</span>typed<span class="token-punctuation">:</span><span class="token-punctuation">:</span><span class="token-punctuation">&lt;</span>Note<span class="token-punctuation">&gt;</span><span class="token-punctuation">(</span><span class="token-string">"notes"</span><span class="token-punctuation">)</span><span class="token-punctuation">?</span><span class="token-punctuation">;</span>

    notes<span class="token-punctuation">.</span><span class="token-function">insert</span><span class="token-punctuation">(</span><span class="token-punctuation">&amp;</span>Note <span class="token-punctuation">{</span> title<span class="token-punctuation">:</span> <span class="token-string">"Buy milk"</span><span class="token-punctuation">.</span><span class="token-function">into</span><span class="token-punctuation">(</span><span class="token-punctuation">)</span><span class="token-punctuation">,</span> pinned<span class="token-punctuation">:</span> <span class="token-keyword">false</span> <span class="token-punctuation">}</span><span class="token-punctuation">)</span><span class="token-punctuation">?</span><span class="token-punctuation">;</span>
    <span class="token-keyword">let</span> pinned<span class="token-punctuation">:</span> Vec<span class="token-punctuation">&lt;</span>Note<span class="token-punctuation">&gt;</span> <span class="token-operator">=</span> notes<span class="token-punctuation">.</span><span class="token-function">find</span><span class="token-punctuation">(</span>json<span class="token-punctuation">!</span><span class="token-punctuation">(</span><span class="token-punctuation">{</span> <span class="token-string">"pinned"</span><span class="token-punctuation">:</span> <span class="token-keyword">true</span> <span class="token-punctuation">}</span><span class="token-punctuation">)</span><span class="token-punctuation">)</span><span class="token-punctuation">?</span><span class="token-punctuation">;</span>
    Ok<span class="token-punctuation">(</span><span class="token-punctuation">(</span><span class="token-punctuation">)</span><span class="token-punctuation">)</span>
<span class="token-punctuation">}</span>`,
      },
    ],
  },
}

const currentTab = computed(() => allTabs[activeTab.value])
</script>

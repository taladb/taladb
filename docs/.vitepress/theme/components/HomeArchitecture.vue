<template>
  <section class="tdb-section tdb-section--alt" aria-labelledby="architecture-title">
    <div class="tdb-container">
      <header class="tdb-section-head">
        <p class="tdb-eyebrow">Architecture</p>
        <h2 id="architecture-title" class="tdb-h2">One Rust engine, a native binding per platform</h2>
        <p class="tdb-lede">
          Each platform has a thin binding over the same <code>taladb</code> crate, so
          filters, indexes and search work the same way everywhere. Storage and API
          style follow each platform's conventions.
        </p>
      </header>

      <figure class="arch" aria-label="TalaDB architecture: six platform packages over one Rust engine and storage layer">
        <div class="arch-groups">
          <div v-for="group in groups" :key="group.label" class="arch-group">
            <p class="arch-group-label">
              {{ group.label }}
              <code v-if="group.pkg">{{ group.pkg }}</code>
            </p>
            <ul class="arch-platforms" role="list">
              <li v-for="p in group.platforms" :key="p.name">
                <a :href="p.guide" class="arch-card">
                  <span class="arch-card-name">{{ p.name }}</span>
                  <code class="arch-card-pkg">{{ p.pkg }}</code>
                  <span class="arch-card-via">{{ p.via }}</span>
                </a>
              </li>
            </ul>
          </div>
        </div>

        <div class="arch-link" aria-hidden="true"></div>

        <div class="arch-core">
          <div class="arch-core-head">
            <span class="arch-core-name">TalaDB engine</span>
            <span class="arch-core-meta">Rust · crate <code>taladb</code></span>
          </div>
          <ul class="arch-chips" role="list">
            <li v-for="c in core" :key="c">{{ c }}</li>
          </ul>
        </div>

        <div class="arch-storage">
          <span class="arch-storage-name">Storage</span>
          <span>
            redb B-tree files on Node.js, React Native, Android, iOS and Rust ·
            OPFS in the browser, IndexedDB snapshot fallback
          </span>
        </div>

        <figcaption class="arch-caption">
          Select a platform to open its guide. The Kotlin and Swift packages live in
          <a href="https://github.com/taladb/taladb-kotlin" target="_blank" rel="noopener">taladb-kotlin</a>
          and
          <a href="https://github.com/taladb/taladb-swift" target="_blank" rel="noopener">taladb-swift</a>.
        </figcaption>
      </figure>
    </div>
  </section>
</template>

<script setup lang="ts">
// Mechanisms mirror README.md's platform table and /introduction.
const groups = [
  {
    label: 'JavaScript & TypeScript: one API from',
    pkg: 'taladb',
    platforms: [
      { name: 'Browser', pkg: '@taladb/web', via: 'WebAssembly in a worker', guide: '/guide/web' },
      { name: 'Node.js', pkg: '@taladb/node', via: 'napi-rs native module', guide: '/guide/node' },
      { name: 'React Native', pkg: '@taladb/react-native', via: 'JSI over the C FFI', guide: '/guide/react-native' },
    ],
  },
  {
    label: 'Native: idiomatic per-language APIs',
    pkg: '',
    platforms: [
      { name: 'Android', pkg: 'dev.taladb:taladb-android', via: 'Kotlin · JNI over the C FFI', guide: '/guide/android' },
      { name: 'iOS & macOS', pkg: 'taladb-swift', via: 'Swift · SwiftPM over the C FFI', guide: '/guide/swift' },
      { name: 'Rust', pkg: 'taladb', via: 'The engine crate itself', guide: '/guide/rust' },
    ],
  },
]

const core = [
  'Documents & collections',
  'Filters & aggregation',
  'Secondary indexes',
  'Vector search (flat, HNSW)',
  'BM25 full-text',
  'Hybrid search (RRF)',
  'ACID transactions',
  'Migrations',
  'Live queries',
  'Encryption at rest',
]
</script>

<style scoped>
.arch {
  margin: 0;
}

.arch-groups {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.arch-group {
  padding: 16px;
  border: 1px dashed var(--vp-c-border);
  border-radius: var(--tdb-radius-lg);
}

.arch-group-label {
  margin: 0 0 12px;
  font-size: 0.8125rem;
  color: var(--vp-c-text-2);
}

.arch-group-label code,
.arch-core-meta code {
  font-family: var(--vp-font-family-mono);
  font-size: 0.75rem;
  color: var(--vp-c-text-1);
}

.arch-platforms {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 8px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.arch-card {
  display: flex;
  flex-direction: column;
  gap: 4px;
  height: 100%;
  min-height: 44px;
  padding: 12px;
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--tdb-radius);
  background: var(--vp-c-bg);
  text-decoration: none;
  transition: border-color 0.15s;
}

.arch-card:hover {
  border-color: var(--vp-c-brand-1);
}

.arch-card-name {
  font-size: 0.9375rem;
  font-weight: 600;
  color: var(--vp-c-text-1);
}

.arch-card-pkg {
  font-family: var(--vp-font-family-mono);
  font-size: 0.6875rem;
  color: var(--vp-c-brand-1);
  overflow-wrap: anywhere;
}

.arch-card-via {
  font-size: 0.75rem;
  line-height: 1.4;
  color: var(--vp-c-text-3);
}

.arch-link {
  width: 2px;
  height: 28px;
  margin: 0 auto;
  background: var(--vp-c-border);
}

.arch-core {
  padding: 20px;
  border-radius: var(--tdb-radius-lg);
  background: var(--tdb-ink-surface);
  color: #f0eee8;
}

.arch-core-head {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  justify-content: space-between;
  gap: 8px;
  margin-bottom: 14px;
}

.arch-core-name {
  font-size: 1.0625rem;
  font-weight: 600;
}

.arch-core-meta {
  font-size: 0.8125rem;
  color: #aab1be;
}

.arch-core-meta code {
  color: var(--tdb-blue-300);
}

.arch-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.arch-chips li {
  padding: 4px 10px;
  border: 1px solid #2c3440;
  border-radius: 999px;
  font-size: 0.8125rem;
  color: #d6d9e0;
}

.arch-storage {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 12px;
  margin-top: 8px;
  padding: 12px 20px;
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--tdb-radius);
  background: var(--vp-c-bg);
  font-size: 0.8125rem;
  line-height: 1.5;
  color: var(--vp-c-text-2);
}

.arch-storage-name {
  font-weight: 600;
  color: var(--vp-c-text-1);
}

.arch-caption {
  margin-top: 16px;
  font-size: 0.8125rem;
  color: var(--vp-c-text-3);
}

.arch-caption a {
  color: var(--vp-c-brand-1);
}

.dark .arch-core {
  border: 1px solid var(--vp-c-border);
  background: var(--vp-c-bg-soft);
}

@media (max-width: 1024px) {
  .arch-groups {
    grid-template-columns: minmax(0, 1fr);
  }
}

@media (max-width: 560px) {
  .arch-group {
    padding: 12px;
  }

  .arch-platforms {
    grid-template-columns: minmax(0, 1fr);
  }

  .arch-card {
    display: grid;
    grid-template-columns: auto 1fr;
    column-gap: 12px;
    align-items: baseline;
  }

  .arch-card-pkg {
    justify-self: end;
    text-align: right;
  }

  .arch-card-via {
    grid-column: 1 / -1;
  }
}
</style>

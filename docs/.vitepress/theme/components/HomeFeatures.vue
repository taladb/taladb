<template>
  <section class="tdb-section" aria-labelledby="capabilities-title">
    <div class="tdb-container">
      <header class="tdb-section-head">
        <p class="tdb-eyebrow">Capabilities</p>
        <h2 id="capabilities-title" class="tdb-h2">Documents, queries and search in one engine</h2>
        <p class="tdb-lede">
          Keep records, metadata and embeddings together, and query all of them
          in-process, with no separate search service or vector store to run.
        </p>
      </header>

      <ul class="caps" role="list">
        <li v-for="cap in capabilities" :key="cap.title" class="cap">
          <h3 class="cap-title">{{ cap.title }}</h3>
          <p class="cap-desc">{{ cap.desc }}</p>
          <a :href="cap.link" class="tdb-link cap-link">
            {{ cap.linkText }}<span class="visually-hidden">: {{ cap.title }}</span> →
          </a>
        </li>
      </ul>
    </div>
  </section>
</template>

<script setup lang="ts">
// Every claim here is backed by the linked page; keep them in sync.
const capabilities = [
  {
    title: 'Documents and structured queries',
    desc: 'Store JSON documents in collections. Query them with MongoDB-style filters, updates, sorting and aggregation pipelines. A query planner uses your secondary indexes automatically.',
    link: '/api/filters',
    linkText: 'Filters',
  },
  {
    title: 'Vector similarity search',
    desc: 'Store embeddings next to the records they describe. Search is exact k-NN by default, with optional HNSW for large collections. Metadata filters apply before ranking, so every one of the top k results matches the filter.',
    link: '/api/vector-search',
    linkText: 'Vector search',
  },
  {
    title: 'Full-text and hybrid search',
    desc: 'Rank keyword matches with BM25. Hybrid search fuses keyword and vector rankings with reciprocal rank fusion, so both exact terms and paraphrases can surface from a single query.',
    link: '/api/search',
    linkText: 'Full-text & hybrid search',
  },
  {
    title: 'Local-first and embedded',
    desc: 'The engine runs inside your app process, with no server to deploy. Data stays on the device and the app keeps working offline. Nothing is sent anywhere unless you opt in, for example with the change webhook.',
    link: '/concepts',
    linkText: 'Core concepts',
  },
  {
    title: 'Transactions and persistence',
    desc: 'Writes are ACID transactions on redb, a pure-Rust B-tree store. Data persists to a file on native platforms and to OPFS in the browser, with an IndexedDB fallback. Optional AES-GCM-256 encryption at rest.',
    link: '/api/encryption',
    linkText: 'Encryption',
  },
  {
    title: 'Live queries and migrations',
    desc: 'Subscribe to a filter and get fresh results after every matching write. Versioned migrations run when the database opens, so your data model can evolve between app releases.',
    link: '/api/live-queries',
    linkText: 'Live queries',
  },
]
</script>

<style scoped>
.caps {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 1px;
  margin: 0;
  padding: 0;
  list-style: none;
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--tdb-radius-lg);
  background: var(--vp-c-divider);
  overflow: hidden;
}

.cap {
  display: flex;
  flex-direction: column;
  padding: 28px;
  background: var(--vp-c-bg);
}

.cap-title {
  margin: 0 0 10px;
  font-size: 1.0625rem;
  font-weight: 600;
  letter-spacing: -0.01em;
  color: var(--vp-c-text-1);
}

.cap-desc {
  flex: 1;
  margin: 0 0 20px;
  font-size: 0.9375rem;
  line-height: 1.65;
  color: var(--vp-c-text-2);
}

.cap-link {
  font-size: 0.875rem;
}

@media (max-width: 960px) {
  .caps {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 640px) {
  .caps {
    grid-template-columns: minmax(0, 1fr);
  }

  .cap {
    padding: 24px 20px;
  }
}
</style>

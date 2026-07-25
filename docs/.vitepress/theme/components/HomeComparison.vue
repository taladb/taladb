<template>
  <section class="home-comparison">
    <div class="comparison-inner">
      <div class="section-header">
        <h2 class="section-title">Why not just use&hellip;?</h2>
        <p class="section-sub">
          Every alternative makes a trade-off TalaDB doesn't.
        </p>
      </div>

      <div class="comparison-table-wrap">
        <table class="comparison-table">
          <thead>
            <tr>
              <th class="col-feature">Capability</th>
              <th class="col-tala">
                <span class="col-tala-label">TalaDB</span>
              </th>
              <th>IndexedDB</th>
              <th>SQLite WASM</th>
              <th>Dexie.js</th>
              <th>Pinecone / Qdrant</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="row in rows" :key="row.capability">
              <td class="col-feature">{{ row.capability }}</td>
              <td class="col-tala">
                <span :class="['cell-val', row.tala === '✓' ? 'yes' : row.tala === '—' ? 'no' : 'partial']">
                  {{ row.tala }}
                </span>
              </td>
              <td>
                <span :class="['cell-val', row.indexed === '✓' ? 'yes' : row.indexed === '—' ? 'no' : 'partial']">
                  {{ row.indexed }}
                </span>
              </td>
              <td>
                <span :class="['cell-val', row.sqlite === '✓' ? 'yes' : row.sqlite === '—' ? 'no' : 'partial']">
                  {{ row.sqlite }}
                </span>
              </td>
              <td>
                <span :class="['cell-val', row.dexie === '✓' ? 'yes' : row.dexie === '—' ? 'no' : 'partial']">
                  {{ row.dexie }}
                </span>
              </td>
              <td>
                <span :class="['cell-val', row.cloud === '✓' ? 'yes' : row.cloud === '—' ? 'no' : 'partial']">
                  {{ row.cloud }}
                </span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <p class="comparison-footnote">
        ✓ supported &nbsp;·&nbsp; ~ partial / limited &nbsp;·&nbsp; — not supported
      </p>
    </div>
  </section>
</template>

<script setup lang="ts">
const rows = [
  { capability: 'Document (JSON) storage',         tala: '✓', indexed: '✓',  sqlite: '~', dexie: '✓',  cloud: '✓'  },
  { capability: 'MongoDB-style query DSL',          tala: '✓', indexed: '—',  sqlite: '—', dexie: '~',  cloud: '~'  },
  { capability: 'Aggregation pipeline ($group)',    tala: '✓', indexed: '—',  sqlite: '✓', dexie: '—',  cloud: '—'  },
  { capability: 'On-device vector search',          tala: '✓', indexed: '—',  sqlite: '~', dexie: '—',  cloud: '✓'  },
  { capability: 'Filtered vector search',   tala: '✓', indexed: '—',  sqlite: '—', dexie: '—',  cloud: '✓'  },
  { capability: 'TypeScript generics on queries',   tala: '✓', indexed: '—',  sqlite: '—', dexie: '✓',  cloud: '—'  },
  { capability: 'ACID transactions',                tala: '✓', indexed: '~',  sqlite: '✓', dexie: '~',  cloud: '✓'  },
  { capability: 'Secondary B-tree indexes',         tala: '✓', indexed: '✓',  sqlite: '✓', dexie: '✓',  cloud: '~'  },
  { capability: 'Full-text search (BM25)',          tala: '✓', indexed: '—',  sqlite: '~', dexie: '~',  cloud: '~'  },
  { capability: 'Hybrid search (keyword + vector)', tala: '✓', indexed: '—',  sqlite: '—', dexie: '—',  cloud: '~'  },
  { capability: 'Live queries / subscriptions',     tala: '✓', indexed: '—',  sqlite: '—', dexie: '~',  cloud: '—'  },
  { capability: 'Bidirectional sync (LWW / CRDT)',  tala: '✓', indexed: '—',  sqlite: '—', dexie: '~',  cloud: '—'  },
  { capability: 'Encryption at rest',               tala: '✓', indexed: '—',  sqlite: '~', dexie: '—',  cloud: '✓'  },
  { capability: 'Schema migrations',                tala: '✓', indexed: '—',  sqlite: '—', dexie: '✓',  cloud: '—'  },
  { capability: 'React Native (JSI)',               tala: '✓', indexed: '—',  sqlite: '✓', dexie: '—',  cloud: '—'  },
  { capability: 'Browser + OPFS persistence',       tala: '✓', indexed: '✓',  sqlite: '✓', dexie: '✓',  cloud: '—'  },
  { capability: 'No cloud / no API key',            tala: '✓', indexed: '✓',  sqlite: '✓', dexie: '✓',  cloud: '—'  },
  { capability: 'Rust core (no GC pauses)',         tala: '✓', indexed: '—',  sqlite: '~', dexie: '—',  cloud: '✓'  },
  { capability: 'sub-400 KB WASM bundle',           tala: '✓', indexed: '—',  sqlite: '—', dexie: '—',  cloud: '—'  },
]
</script>

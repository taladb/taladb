<template>
  <section class="home-features">
    <div class="section-container">
      <div class="section-header">
        <h2 class="section-title">Everything you need. Nothing you don't.</h2>
        <p class="section-sub">
          A vector and document database in one embedded engine — browser, Node.js, and React Native.
        </p>
      </div>

      <div class="features-grid">
        <div v-for="feature in features" :key="feature.title" class="feature-card">
          <div class="feature-icon" :style="{ background: feature.iconBg }">
            <span class="feature-icon-inner" v-html="feature.icon"></span>
          </div>
          <h3 class="feature-title">{{ feature.title }}</h3>
          <p class="feature-desc">{{ feature.desc }}</p>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
const features = [
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M12 1v4M12 19v4M4.22 4.22l2.83 2.83M16.95 16.95l2.83 2.83M1 12h4M19 12h4M4.22 19.78l2.83-2.83M16.95 7.05l2.83-2.83"/></svg>`,
    iconBg: 'rgba(181,75,49,0.12)',
    title: 'On-Device Vector Search',
    desc: 'Exact k-NN (cosine, dot, euclidean) with metadata pre-filtering, plus optional HNSW at scale — the filtered vector search cloud providers charge for, running entirely on device. No API key, no round-trip.',
  },
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="7"/><line x1="21" y1="21" x2="16.65" y2="16.65"/><path d="M8 11h6M11 8v6"/></svg>`,
    iconBg: 'rgba(181,75,49,0.1)',
    title: 'Hybrid Search (RAG-Ready)',
    desc: 'Fuse BM25 keyword ranking with vector similarity via reciprocal rank fusion — so an exact SKU and a semantic paraphrase both surface from one query. The production RAG retrieval recipe, on-device.',
  },
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M21 12c0 1.66-4 3-9 3s-9-1.34-9-3"/><path d="M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5"/></svg>`,
    iconBg: 'rgba(59,130,246,0.1)',
    title: 'MongoDB-like Query API',
    desc: 'Familiar $eq, $gt, $in, $and, $or operators. Fully typed with TypeScript generics — field names and value types validated at compile time.',
  },
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="22 12 18 12 15 21 9 3 6 12 2 12"/></svg>`,
    iconBg: 'rgba(16,185,129,0.1)',
    title: 'Live Queries',
    desc: 'Subscribe to a filtered collection and receive a fresh snapshot on every write — no polling, no websockets, no infrastructure.',
  },
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>`,
    iconBg: 'rgba(139,92,246,0.1)',
    title: 'Encryption at Rest',
    desc: 'Transparent AES-GCM-256 encryption. Keys derived via PBKDF2-HMAC-SHA256. Every value encrypted with a fresh random nonce — tamper-proof and crash-safe.',
  },
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>`,
    iconBg: 'rgba(245,158,11,0.1)',
    title: 'ACID Transactions',
    desc: 'Powered by redb — a pure-Rust B-tree storage engine. Every write is atomic, consistent, isolated, and durable. Crash-safe out of the box.',
  },
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="3" width="6" height="6"/><rect x="16" y="3" width="6" height="6"/><rect x="9" y="16" width="6" height="6"/><path d="M5 9v4h14V9"/><path d="M12 13v3"/></svg>`,
    iconBg: 'rgba(20,184,166,0.1)',
    title: 'Runs Everywhere',
    desc: 'One Rust core compiles to browser WASM, a Node.js native module, and a React Native JSI HostObject. One API, zero platform branches in your app code.',
  },
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="8" y1="6" x2="21" y2="6"/><line x1="8" y1="12" x2="21" y2="12"/><line x1="8" y1="18" x2="21" y2="18"/><line x1="3" y1="6" x2="3.01" y2="6"/><line x1="3" y1="12" x2="3.01" y2="12"/><line x1="3" y1="18" x2="3.01" y2="18"/></svg>`,
    iconBg: 'rgba(239,68,68,0.1)',
    title: 'Secondary Indexes',
    desc: 'Type-safe B-tree indexes with O(log n) range scans. The query planner picks the best index automatically — no hints or query annotations needed.',
  },
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14.5 10c-.83 0-1.5-.67-1.5-1.5v-5c0-.83.67-1.5 1.5-1.5s1.5.67 1.5 1.5v5c0 .83-.67 1.5-1.5 1.5z"/><path d="M20.5 10H19V8.5c0-.83.67-1.5 1.5-1.5s1.5.67 1.5 1.5-.67 1.5-1.5 1.5z"/><path d="M9.5 14c.83 0 1.5.67 1.5 1.5v5c0 .83-.67 1.5-1.5 1.5S8 21.33 8 20.5v-5c0-.83.67-1.5 1.5-1.5z"/><path d="M3.5 14H5v1.5c0 .83-.67 1.5-1.5 1.5S2 16.33 2 15.5 2.67 14 3.5 14z"/><path d="M14 14.5c0-.83.67-1.5 1.5-1.5h5c.83 0 1.5.67 1.5 1.5s-.67 1.5-1.5 1.5h-5c-.83 0-1.5-.67-1.5-1.5z"/><path d="M15.5 19H14v1.5c0 .83.67 1.5 1.5 1.5s1.5-.67 1.5-1.5-.67-1.5-1.5-1.5z"/><path d="M10 9.5C10 8.67 9.33 8 8.5 8h-5C2.67 8 2 8.67 2 9.5S2.67 11 3.5 11h5c.83 0 1.5-.67 1.5-1.5z"/><path d="M8.5 5H10V3.5C10 2.67 9.33 2 8.5 2S7 2.67 7 3.5 7.67 5 8.5 5z"/></svg>`,
    iconBg: 'rgba(99,102,241,0.1)',
    title: 'Full-Text Search (BM25)',
    desc: 'Relevance-ranked keyword search with searchText — the same BM25 model as Lucene and Elasticsearch — plus the boolean $contains filter. No external search engine required.',
  },
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="4" width="14" height="10" rx="2"/><path d="M8 18h12a2 2 0 0 0 2-2V8"/><path d="M18 22l4-4-4-4"/></svg>`,
    iconBg: 'rgba(14,165,233,0.1)',
    title: 'Multi-Tab Safe',
    desc: 'The first browser tab owns the OPFS file; later tabs read a live copy and forward their writes to it. No duplicate engines, no lost updates, no lock errors — and db.isPrimary() tells you which tab owns the storage when work must run only once.',
  },
  {
    icon: `<svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="20" x2="18" y2="10"/><line x1="12" y1="20" x2="12" y2="4"/><line x1="6" y1="20" x2="6" y2="14"/></svg>`,
    iconBg: 'rgba(217,70,239,0.1)',
    title: 'Aggregation Pipelines',
    desc: 'MongoDB-style $match, $group, $sort, $skip, $limit, and $project stages with $sum, $avg, $min, $max accumulators — summaries computed inside the Rust engine, not by materialising every document in JavaScript.',
  },
]
</script>

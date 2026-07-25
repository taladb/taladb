<template>
  <section class="home-usecases">
    <div class="section-container">
      <div class="section-header">
        <h2 class="section-title">Built for AI that runs on the device</h2>
        <p class="section-sub">
          From AI-powered web apps to offline-first mobile — TalaDB fits where your data lives.
        </p>
      </div>

      <div class="usecases-grid">
        <div v-for="uc in useCases" :key="uc.title" class="usecase-card">
          <div class="usecase-header">
            <div class="usecase-icon-wrap" :style="{ background: uc.iconBg }">
              <span v-html="uc.icon"></span>
            </div>
            <div class="usecase-platform-tags">
              <span v-for="p in uc.platforms" :key="p" class="platform-tag">{{ p }}</span>
            </div>
          </div>
          <h3 class="usecase-title">{{ uc.title }}</h3>
          <p class="usecase-desc">{{ uc.desc }}</p>
          <ul class="usecase-bullets">
            <li v-for="b in uc.bullets" :key="b">{{ b }}</li>
          </ul>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
const useCases = [
  {
    icon: `<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M12 1v4M12 19v4M4.22 4.22l2.83 2.83M16.95 16.95l2.83 2.83M1 12h4M19 12h4M4.22 19.78l2.83-2.83M16.95 7.05l2.83-2.83"/></svg>`,
    iconBg: 'rgba(181,75,49,0.1)',
    title: 'AI-Powered Web Apps',
    platforms: ['Browser', 'WASM', 'OPFS'],
    desc: 'Pair TalaDB with on-device models (transformers.js, ONNX Web) to build semantic search, RAG pipelines, and AI assistants that run entirely in the browser.',
    bullets: [
      'Store document embeddings alongside metadata',
      'Filtered search: filter by tag, rank by similarity',
      'No API key, no rate limit, no data egress',
    ],
  },
  {
    icon: `<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>`,
    iconBg: 'rgba(59,130,246,0.1)',
    title: 'Offline-First SaaS',
    platforms: ['Browser', 'Node.js'],
    desc: 'Build apps that work without a connection. TalaDB persists data to OPFS in the browser and redb on Node.js — sync to your backend whenever a connection is available.',
    bullets: [
      'ACID writes survive tab closes and crashes',
      'HTTP push sync fires in the background',
      'Live queries keep the UI reactive without polling',
    ],
  },
  {
    icon: `<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="2" width="14" height="20" rx="2" ry="2"/><line x1="12" y1="18" x2="12.01" y2="18"/></svg>`,
    iconBg: 'rgba(16,185,129,0.1)',
    title: 'React Native Mobile',
    platforms: ['iOS', 'Android', 'JSI'],
    desc: 'The JSI HostObject gives you synchronous database access without bridges. Store user data, search history, and embeddings directly on the device.',
    bullets: [
      'Synchronous reads via JSI — no async overhead',
      'Same TypeScript API as web and Node.js',
      'AES-GCM-256 encryption for sensitive user data',
    ],
  },
  {
    icon: `<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>`,
    iconBg: 'rgba(139,92,246,0.1)',
    title: 'CLI Tools & Scripts',
    platforms: ['Node.js', 'Bun'],
    desc: 'Use TalaDB as a fast, embedded document store for CLI tools, build pipelines, and data transformation scripts — no database server to spin up.',
    bullets: [
      'Native napi-rs module — no serialization overhead',
      'ACID transactions for safe concurrent writes',
      'Full-text and vector search in a single binary',
    ],
  },
]
</script>

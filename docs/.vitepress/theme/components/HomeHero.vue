<template>
  <section class="hero" aria-labelledby="hero-title">
    <div class="tdb-container hero-grid">
      <div class="hero-copy">
        <a class="hero-release" :href="releaseUrl" target="_blank" rel="noopener">
          <span class="hero-release-tag">{{ version }}</span>
          <span>Open source · MIT or Apache-2.0</span>
        </a>

        <h1 id="hero-title" class="hero-title">
          One database.
          <span class="hero-title-accent">Everywhere your app runs.</span>
        </h1>

        <p class="hero-lede">
          TalaDB is an embedded document and vector database built on a Rust core.
          Store JSON documents, query them with filters and indexes, and run vector,
          full-text and hybrid search inside your app — in the browser, on Node.js,
          and in React Native, Android and iOS apps. No server, no cloud round-trip.
        </p>

        <div class="hero-actions">
          <a href="/introduction" class="tdb-btn tdb-btn--primary">Get started</a>
          <a
            href="https://github.com/taladb/taladb"
            class="tdb-btn tdb-btn--secondary"
            target="_blank"
            rel="noopener"
          >
            <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
              <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8z" />
            </svg>
            GitHub
          </a>
          <a
            href="https://demo-web.taladb.dev/"
            class="tdb-link hero-demo"
            target="_blank"
            rel="noopener"
          >Try the web demo →</a>
        </div>

        <div class="hero-install">
          <span class="hero-install-label">Browser</span>
          <code class="hero-install-cmd">{{ installCmd }}</code>
          <button
            type="button"
            class="hero-install-copy"
            :aria-label="copied ? 'Copied' : 'Copy install command'"
            @click="copy"
          >
            <svg v-if="!copied" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
            <svg v-else width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><polyline points="20 6 9 17 4 12" /></svg>
          </button>
          <span class="visually-hidden" aria-live="polite">{{ copied ? 'Copied to clipboard' : '' }}</span>
        </div>
      </div>

      <div class="hero-code tdb-code">
        <div class="tdb-code-head">
          <span>app.ts</span>
          <span class="hero-code-note"><code>embed()</code> = any embedding model</span>
        </div>
        <div class="vp-doc"><slot /></div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref } from 'vue'
// Named import: Vite ships only this field, not the whole package.json.
import { version as pkgVersion } from '../../../../package.json'

const version = `v${pkgVersion}`
const releaseUrl = `https://github.com/taladb/taladb/releases/tag/${version}`
const installCmd = 'pnpm add taladb @taladb/web'

const copied = ref(false)
function copy() {
  navigator.clipboard?.writeText(installCmd).then(() => {
    copied.value = true
    setTimeout(() => (copied.value = false), 2000)
  })
}
</script>

<style scoped>
.hero {
  padding: 72px var(--tdb-gutter) var(--tdb-section-y);
}

.hero-grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 48px;
  align-items: center;
}

.hero-release {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 24px;
  padding: 4px 12px 4px 4px;
  border: 1px solid var(--vp-c-divider);
  border-radius: 999px;
  font-size: 0.8125rem;
  color: var(--vp-c-text-2);
  text-decoration: none;
  transition: border-color 0.15s;
}

.hero-release:hover {
  border-color: var(--vp-c-text-3);
}

.hero-release-tag {
  padding: 2px 8px;
  border-radius: 999px;
  background: var(--vp-c-brand-soft);
  color: var(--vp-c-brand-1);
  font-family: var(--vp-font-family-mono);
  font-size: 0.75rem;
  font-weight: 500;
}

.hero-title {
  margin: 0 0 20px;
  font-size: clamp(2.25rem, 4.2vw, 3.25rem);
  font-weight: 600;
  line-height: 1.08;
  letter-spacing: -0.035em;
  color: var(--vp-c-text-1);
}

.hero-title-accent {
  display: block;
  color: var(--vp-c-brand-1);
  text-wrap: balance;
}

.hero-lede {
  max-width: 540px;
  margin: 0 0 32px;
  font-size: 1.125rem;
  line-height: 1.65;
  color: var(--vp-c-text-2);
}

.hero-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px 16px;
  margin-bottom: 28px;
}

.hero-demo {
  font-size: 0.9375rem;
  padding: 10px 4px;
}

.hero-install {
  display: inline-flex;
  align-items: center;
  max-width: 100%;
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--tdb-radius-sm);
  background: var(--vp-c-bg-alt);
  font-family: var(--vp-font-family-mono);
  font-size: 0.8125rem;
}

.hero-install-label {
  padding: 8px 10px 8px 12px;
  border-right: 1px solid var(--vp-c-divider);
  color: var(--vp-c-text-3);
  font-size: 0.75rem;
}

.hero-install-cmd {
  padding: 8px 12px;
  color: var(--vp-c-text-1);
  white-space: nowrap;
  overflow-x: auto;
  min-width: 0;
}

.hero-install-copy {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 40px;
  align-self: stretch;
  border-left: 1px solid var(--vp-c-divider);
  color: var(--vp-c-text-3);
  transition: color 0.15s;
}

.hero-install-copy:hover {
  color: var(--vp-c-text-1);
}

.hero-code {
  box-shadow: var(--tdb-shadow);
}

.hero-code-note {
  color: var(--vp-c-text-3);
  font-family: var(--vp-font-family-base);
}

@media (max-width: 960px) {
  .hero {
    padding-top: 48px;
  }

  .hero-grid {
    grid-template-columns: minmax(0, 1fr);
    gap: 40px;
  }
}

@media (max-width: 480px) {
  .hero-lede {
    font-size: 1.0625rem;
  }

  .hero-actions .tdb-btn {
    flex: 1 1 auto;
  }

  .hero-code-note {
    display: none;
  }
}
</style>

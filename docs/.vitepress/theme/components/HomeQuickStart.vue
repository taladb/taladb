<template>
  <section class="tdb-section" aria-labelledby="quickstart-title">
    <div class="tdb-container">
      <header class="tdb-section-head">
        <p class="tdb-eyebrow">Quickstart</p>
        <h2 id="quickstart-title" class="tdb-h2">Install, open, query</h2>
        <p class="tdb-lede">
          Pick a platform. Each example is the minimal setup from that platform's
          guide, which covers bundler, build and permission details.
        </p>
      </header>

      <div class="qs">
        <div class="qs-tabs" role="tablist" aria-label="Platform">
          <button
            v-for="(tab, i) in tabs"
            :id="`qs-tab-${tab.id}`"
            :key="tab.id"
            ref="tabEls"
            type="button"
            role="tab"
            class="qs-tab"
            :class="{ active: active === i }"
            :aria-selected="active === i"
            :aria-controls="`qs-panel-${tab.id}`"
            :tabindex="active === i ? 0 : -1"
            @click="active = i"
            @keydown="onKey($event, i)"
          >
            {{ tab.label }}
          </button>
        </div>

        <!-- Panels are filled from index.md slots, so code is highlighted by
             VitePress (Shiki) and gets its copy button and theme colours. -->
        <div
          v-for="(tab, i) in tabs"
          v-show="active === i"
          :id="`qs-panel-${tab.id}`"
          :key="tab.id"
          role="tabpanel"
          class="qs-panel tdb-code"
          :aria-labelledby="`qs-tab-${tab.id}`"
          tabindex="0"
        >
          <div class="vp-doc"><slot :name="tab.id" /></div>
          <div class="qs-foot">
            <a :href="tab.guide" class="tdb-link">{{ tab.label }} guide →</a>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const tabs = [
  { id: 'web', label: 'Browser', guide: '/guide/web' },
  { id: 'node', label: 'Node.js', guide: '/guide/node' },
  { id: 'rn', label: 'React Native', guide: '/guide/react-native' },
  { id: 'android', label: 'Android', guide: '/guide/android' },
  { id: 'swift', label: 'iOS & macOS', guide: '/guide/swift' },
  { id: 'rust', label: 'Rust', guide: '/guide/rust' },
]

const active = ref(0)
const tabEls = ref<HTMLButtonElement[]>([])

// WAI-ARIA tabs pattern: arrows move between tabs, Home/End jump.
function onKey(e: KeyboardEvent, i: number) {
  const last = tabs.length - 1
  let next = i
  if (e.key === 'ArrowRight') next = i === last ? 0 : i + 1
  else if (e.key === 'ArrowLeft') next = i === 0 ? last : i - 1
  else if (e.key === 'Home') next = 0
  else if (e.key === 'End') next = last
  else return
  e.preventDefault()
  active.value = next
  tabEls.value[next]?.focus()
}
</script>

<style scoped>
.qs {
  max-width: 880px;
}

.qs-tabs {
  display: flex;
  gap: 4px;
  margin-bottom: 16px;
  padding: 4px;
  border: 1px solid var(--vp-c-divider);
  border-radius: var(--tdb-radius);
  background: var(--vp-c-bg-alt);
  overflow-x: auto;
  scrollbar-width: none;
}

.qs-tabs::-webkit-scrollbar {
  display: none;
}

.qs-tab {
  flex: 1 0 auto;
  min-height: 40px;
  padding: 0 14px;
  border-radius: var(--tdb-radius-sm);
  font-size: 0.875rem;
  font-weight: 500;
  color: var(--vp-c-text-2);
  white-space: nowrap;
  transition:
    background-color 0.15s,
    color 0.15s;
}

.qs-tab:hover {
  color: var(--vp-c-text-1);
}

.qs-tab.active {
  background: var(--vp-c-bg);
  color: var(--vp-c-text-1);
  font-weight: 600;
  box-shadow: 0 1px 2px rgba(20, 23, 27, 0.08);
}

.dark .qs-tab.active {
  background: var(--vp-c-bg-soft);
}

.qs-panel :deep(.qs-step) {
  margin: 0;
  padding: 14px 16px 0;
  font-family: var(--vp-font-family-base);
  font-size: 0.8125rem;
  font-weight: 600;
  color: var(--vp-c-text-2);
}

.qs-panel :deep(.qs-step + div[class*='language-']) {
  border-top: 0;
}

.qs-panel :deep(.qs-step:not(:first-child)) {
  border-top: 1px solid var(--vp-c-divider);
}

.qs-foot {
  padding: 12px 16px;
  border-top: 1px solid var(--vp-c-divider);
  font-size: 0.875rem;
}
</style>

<template>
  <section class="tdb-section tdb-section--alt" aria-labelledby="cta-title">
    <div class="tdb-container cta">
      <div class="cta-copy">
        <h2 id="cta-title" class="tdb-h2">Open source, built in the open</h2>
        <p class="tdb-lede">
          TalaDB is dual-licensed under MIT or Apache-2.0. Read the docs, try a demo,
          or open an issue or pull request on GitHub.
        </p>
        <div class="cta-actions">
          <a href="/introduction" class="tdb-btn tdb-btn--primary">Read the docs</a>
          <a
            href="https://github.com/taladb/taladb"
            class="tdb-btn tdb-btn--secondary"
            target="_blank"
            rel="noopener"
          >Star on GitHub</a>
        </div>
        <p class="cta-sponsor">
          TalaDB is maintained by one person.
          <a href="https://github.com/sponsors/tala-sh" target="_blank" rel="noopener">Sponsoring on GitHub</a>
          funds continued development.
        </p>
      </div>

      <nav class="cta-links" aria-label="Project links">
        <div v-for="col in columns" :key="col.title">
          <h3 class="cta-links-title">{{ col.title }}</h3>
          <ul role="list">
            <li v-for="l in col.links" :key="l.text">
              <a
                :href="l.href"
                :target="l.href.startsWith('http') ? '_blank' : undefined"
                :rel="l.href.startsWith('http') ? 'noopener' : undefined"
              >{{ l.text }}</a>
            </li>
          </ul>
        </div>
      </nav>
    </div>
  </section>
</template>

<script setup lang="ts">
const columns = [
  {
    title: 'Live demos',
    links: [
      { text: 'Web demo', href: 'https://demo-web.taladb.dev/' },
      { text: 'React Native (Google Play)', href: 'https://play.google.com/store/apps/details?id=dev.thinkgrid.kepta' },
      { text: 'Android, Kotlin (Google Play)', href: 'https://play.google.com/store/apps/details?id=dev.thinkgrid.notewell' },
    ],
  },
  {
    title: 'Project',
    links: [
      { text: 'Changelog', href: 'https://github.com/taladb/taladb/releases' },
      { text: 'Roadmap', href: '/roadmap' },
      { text: 'Contributing', href: 'https://github.com/taladb/taladb/blob/main/CONTRIBUTING.md' },
      { text: 'Discussions', href: 'https://github.com/taladb/taladb/discussions' },
    ],
  },
]
</script>

<style scoped>
.cta {
  display: grid;
  grid-template-columns: minmax(0, 1.3fr) minmax(0, 1fr);
  gap: 48px;
  align-items: start;
}

.cta-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  margin: 28px 0 20px;
}

.cta-sponsor {
  margin: 0;
  font-size: 0.875rem;
  color: var(--vp-c-text-3);
}

.cta-sponsor a {
  color: var(--vp-c-text-2);
  text-decoration: underline;
  text-underline-offset: 2px;
}

.cta-links {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 24px;
}

.cta-links-title {
  margin: 0 0 12px;
  font-size: 0.8125rem;
  font-weight: 600;
  color: var(--vp-c-text-1);
}

.cta-links ul {
  margin: 0;
  padding: 0;
  list-style: none;
}

.cta-links a {
  display: inline-block;
  padding: 6px 0;
  font-size: 0.875rem;
  color: var(--vp-c-text-2);
  text-decoration: none;
}

.cta-links a:hover {
  color: var(--vp-c-brand-1);
}

@media (max-width: 860px) {
  .cta {
    grid-template-columns: minmax(0, 1fr);
    gap: 40px;
  }
}

@media (max-width: 420px) {
  .cta-links {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>

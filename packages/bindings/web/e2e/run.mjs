import { startServer, launch, newTab, createRunner, summarize } from './lib.mjs';

const only = process.argv.slice(2);
const want = (p) => only.length === 0 || only.includes(p);

const server = await startServer();
const { browser, dispose } = await launch();
const all = [];

try {
  if (want('1')) {
    console.log('\n── Phase 1: happy path ──────────────────────────────────');
    const { run } = await import('./phase1-happy.mjs');
    const r = createRunner('happy');
    const page = await newTab(browser, { label: 'A' });
    await run(page, r);
    all.push(...r.results);
    await page.close();
  }

  if (want('2')) {
    console.log('\n── Phase 2: edge cases ──────────────────────────────────');
    const { run } = await import('./phase2-edge.mjs');
    const r = createRunner('edge');
    const page = await newTab(browser, { label: 'A' });
    await run(page, r, browser);
    all.push(...r.results);
    await page.close();
  }

  if (want('3')) {
    console.log('\n── Phase 3: multi-tab ───────────────────────────────────');
    const { run } = await import('./phase3-multitab.mjs');
    const r = createRunner('multitab');
    await run(browser, r);
    all.push(...r.results);
  }

  if (want('4')) {
    console.log('\n── Phase 4: change webhook ──────────────────────────────');
    const { run } = await import('./phase4-webhook.mjs');
    const r = createRunner('webhook');
    await run(browser, r);
    all.push(...r.results);
  }

  if (want('5')) {
    console.log('\n── Phase 5: performance ─────────────────────────────────');
    const { run } = await import('./phase5-perf.mjs');
    const r = createRunner('perf');
    const page = await newTab(browser, { label: 'P' });
    await run(page, r);
    all.push(...r.results);
    await page.close();
  }
} finally {
  const failures = summarize(all);
  await dispose();
  server.kill();
  process.exit(failures ? 1 : 0);
}

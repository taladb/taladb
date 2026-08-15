import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vitest/config'

/**
 * The TanStack Query parity suite — see tests/parity/README.md.
 *
 * Kept in its own config, and out of `vitest.config.ts`'s `include`, because it
 * is *expected to fail* until PLAN-query-parity.md P1–P5 land. A suite that
 * asserts what the library does not do yet is useful; one that blocks every
 * unrelated commit is not. `pnpm test` stays green; `pnpm test:parity` is the
 * scoreboard.
 */
export default defineConfig({
  resolve: {
    alias: [
      {
        // Same rationale as vitest.config.ts: resolve the workspace package to
        // source, since every entry point in its package.json points into a
        // gitignored `dist/`.
        find: /^taladb$/,
        replacement: fileURLToPath(new URL('../taladb/src/index.ts', import.meta.url)),
      },
      {
        // Lets the fixtures import from the specifier a real application would
        // use, so "the only edit is the import line" is literally what the
        // files show rather than something the reader has to take on trust.
        find: /^@taladb\/react\/query$/,
        replacement: fileURLToPath(new URL('./src/query/index.ts', import.meta.url)),
      },
      {
        find: /^@taladb\/react$/,
        replacement: fileURLToPath(new URL('./src/index.ts', import.meta.url)),
      },
    ],
  },
  test: {
    environment: 'jsdom',
    // Unmounts components and clears module-level state between tests; see
    // tests/setup.ts for why forgetting it fails silently.
    setupFiles: ['./tests/setup.ts'],
    include: ['tests/parity/**/*.parity.test.{ts,tsx}'],
  },
})

import { fileURLToPath } from 'node:url'
import { configDefaults, defineConfig } from 'vitest/config'

export default defineConfig({
  resolve: {
    alias: [
      {
        // Resolve the workspace `taladb` package to its source, not its
        // published entry points. Every field in taladb's package.json
        // (`main`, `module`, `exports`) points into `dist/`, which is
        // gitignored — so on a fresh checkout Vite cannot resolve the
        // specifier and every module that touches it fails to transform,
        // before a single test runs. Building taladb first would work but
        // makes `pnpm test` silently grade a stale `dist/`.
        //
        // Exact match only: taladb has no subpath exports, and a bare string
        // alias would prefix-match anything starting with "taladb".
        //
        // Mirrors the `paths` mapping in tsconfig.json so vitest and tsc
        // agree on what `taladb` means.
        find: /^taladb$/,
        replacement: fileURLToPath(new URL('../taladb/src/index.ts', import.meta.url)),
      },
    ],
  },
  test: {
    environment: 'jsdom',
    // Unmounts components and clears module-level state between tests; see
    // tests/setup.ts for why forgetting it fails silently.
    setupFiles: ['./tests/setup.ts'],
    include: ['tests/**/*.test.{ts,tsx}'],
    // The parity suite asserts the surface PLAN-query-parity.md is building
    // towards, so it fails by design until that work lands. It runs from
    // `vitest.parity.config.ts` via `pnpm test:parity`; keeping it out of the
    // default run is what lets it stay honest instead of being skipped.
    exclude: [...configDefaults.exclude, 'tests/parity/**'],
  },
})

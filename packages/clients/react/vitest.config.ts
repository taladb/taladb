import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vitest/config'

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
    include: ['tests/**/*.test.{ts,tsx}'],
  },
})

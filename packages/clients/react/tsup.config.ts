import { defineConfig } from 'tsup'

export default defineConfig({
  // Two entries, not one bundle: `@taladb/react/query` is a separate import
  // path so an app using only the hooks never pulls the query layer into its
  // bundle. tsup keeps the tree under `src/`, so this emits `dist/query/`.
  entry: ['src/index.ts', 'src/query/index.ts'],
  format: ['esm', 'cjs'],
  dts: true,
  outDir: 'dist',
  external: ['react', 'taladb'],
  // React Server Components: mark the whole hooks package as client-side so
  // Next.js apps can import it from any file without tripping the RSC
  // boundary (the SWR / react-query convention). Harmless everywhere else.
  banner: { js: "'use client';" },
})

import { defineConfig, type HeadConfig } from "vitepress";
import pkg from "../../package.json" with { type: "json" };

// The workspace version is kept in sync with every published package by
// `scripts/sync-version.js`, so the nav badge never drifts from the release.
const version = `v${pkg.version}`;

const title = "TalaDB";
const description =
  "TalaDB is an embedded document and vector database with a Rust core. Store documents, run structured queries, and use vector, full-text, and hybrid search in the browser, Node.js, React Native, Android, and iOS. Local-first, no server required.";
const siteUrl = "https://taladb.dev";
const ogImage = `${siteUrl}/tala-db-banner.png`;

export default defineConfig({
  title,
  description,
  base: "/",
  // Internal reports live in docs/reports/ (gitignored). Never build them.
  srcExclude: ["reports/**"],

  transformHead({ pageData }) {
    const canonicalUrl = `${siteUrl}/${pageData.relativePath}`
      .replace(/index\.md$/, "")
      .replace(/\.md$/, "");

    const tags: HeadConfig[] = [
      ["link", { rel: "canonical", href: canonicalUrl }],
    ];

    const pageTitle = pageData.frontmatter.title
      ? `${pageData.frontmatter.title} | ${title}`
      : title;
    const pageDescription = pageData.frontmatter.description ?? description;

    tags.push(
      ["meta", { property: "og:type", content: "website" }],
      ["meta", { property: "og:url", content: canonicalUrl }],
      ["meta", { property: "og:title", content: pageTitle }],
      ["meta", { property: "og:description", content: pageDescription }],
      ["meta", { property: "og:image", content: ogImage }],
      ["meta", { property: "og:site_name", content: title }],

      ["meta", { name: "twitter:card", content: "summary_large_image" }],
      ["meta", { name: "twitter:title", content: pageTitle }],
      ["meta", { name: "twitter:description", content: pageDescription }],
      ["meta", { name: "twitter:image", content: ogImage }],
    );

    return tags;
  },

  head: [
    // Google Analytics
    [
      "script",
      {
        async: "",
        src: "https://www.googletagmanager.com/gtag/js?id=G-SWTD98L8XR",
      },
    ],
    [
      "script",
      {},
      "window.dataLayer=window.dataLayer||[];function gtag(){dataLayer.push(arguments)}gtag('js',new Date());gtag('config','G-SWTD98L8XR');",
    ],
    ["link", { rel: "icon", href: "/favicon.svg", type: "image/svg+xml" }],
    ["link", { rel: "icon", href: "/favicon.png", type: "image/png" }],
    ["link", { rel: "apple-touch-icon", href: "/apple-touch-icon.png" }],
    ["meta", { name: "theme-color", content: "#3157D9" }],
    ["link", { rel: "preconnect", href: "https://api.fontshare.com" }],
    ["link", { rel: "preconnect", href: "https://fonts.googleapis.com" }],
    ["link", { rel: "preconnect", href: "https://fonts.gstatic.com", crossorigin: "" }],
    [
      "link",
      {
        rel: "stylesheet",
        href: "https://api.fontshare.com/v2/css?f[]=general-sans@400,500,600,700&display=swap",
      },
    ],
    [
      "link",
      {
        rel: "stylesheet",
        href: "https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500;600&display=swap",
      },
    ],
    ["meta", { name: "author", content: "taladb" }],
    [
      "meta",
      {
        name: "keywords",
        content:
          "local-first database, rust database, wasm database, react native database, embedded database, document database, vector database, nosql, offline-first, taladb, on-device ai",
      },
    ],
  ],

  sitemap: {
    hostname: siteUrl,
  },

  themeConfig: {
    logo: { light: "/logo.svg", dark: "/logo-dark.svg", alt: "TalaDB" },
    siteTitle: false,

    nav: [
      { text: "Docs", link: "/introduction" },
      { text: "Guides", link: "/guide/web" },
      { text: "API", link: "/api/collection" },
      {
        text: "Demos",
        items: [
          { text: "Web Demo", link: "https://demo-web.taladb.dev/" },
          { text: "React Native Demo", link: "https://play.google.com/store/apps/details?id=dev.thinkgrid.kepta" },
          { text: "Android (Kotlin) Demo", link: "https://play.google.com/store/apps/details?id=dev.thinkgrid.notewell" },
        ],
      },
      {
        text: version,
        items: [
          {
            text: "Changelog",
            link: "https://github.com/taladb/taladb/releases",
          },
          { text: "Roadmap", link: "/roadmap" },
          {
            text: "Contributing",
            link: "https://github.com/taladb/taladb/blob/main/CONTRIBUTING.md",
          },
          {
            text: "npm",
            link: "https://www.npmjs.com/package/taladb",
          },
        ],
      },
    ],

    // Same pages and URLs as before; only grouping and order changed so the
    // reader meets install → concepts → their platform → reference in order.
    sidebar: [
      {
        text: "Getting Started",
        items: [
          { text: "Introduction", link: "/introduction" },
          { text: "Core Concepts", link: "/concepts" },
          { text: "Feature Overview", link: "/features" },
        ],
      },
      {
        text: "Platform Guides",
        items: [
          { text: "Browser (WASM)", link: "/guide/web" },
          { text: "Node.js", link: "/guide/node" },
          { text: "React Native", link: "/guide/react-native" },
          { text: "Android (Kotlin)", link: "/guide/android" },
          { text: "iOS & macOS (Swift)", link: "/guide/swift" },
          { text: "Rust", link: "/guide/rust" },
        ],
      },
      {
        text: "React",
        items: [
          { text: "React Hooks", link: "/guide/react" },
          { text: "Local-First Data (react/query)", link: "/guide/query" },
          { text: "Migrating from TanStack Query", link: "/guide/react-query-migration" },
        ],
      },
      {
        text: "Documents",
        items: [
          { text: "Collection", link: "/api/collection" },
          { text: "Filters", link: "/api/filters" },
          { text: "Updates", link: "/api/updates" },
          { text: "Aggregation", link: "/api/aggregation" },
          { text: "Schema Validation", link: "/api/schema" },
          { text: "Live Queries", link: "/api/live-queries" },
        ],
      },
      {
        text: "Search",
        items: [
          { text: "Vector Search", link: "/api/vector-search" },
          { text: "Full-Text & Hybrid Search", link: "/api/search" },
        ],
      },
      {
        text: "Data & Operations",
        items: [
          { text: "Migrations", link: "/api/migrations" },
          { text: "Encryption", link: "/api/encryption" },
          { text: "Change Webhook", link: "/api/webhook" },
        ],
      },
      {
        text: "Tools & Performance",
        collapsed: true,
        items: [
          { text: "CLI Dev Tools", link: "/guide/cli" },
          { text: "Vector Benchmarks", link: "/guide/vector-benchmarks" },
          { text: "Roadmap", link: "/roadmap" },
        ],
      },
    ],

    socialLinks: [
      { icon: "github", link: "https://github.com/taladb/taladb" },
    ],

    footer: {
      message: `
        <a href="/introduction">Docs</a> ·
        <a href="https://www.npmjs.com/package/taladb" target="_blank" rel="noopener">npm</a> ·
        <a href="/roadmap">Roadmap</a> ·
        <a href="https://github.com/taladb/taladb/discussions" target="_blank" rel="noopener">Discussions</a> ·
        <a href="https://github.com/taladb/taladb/releases" target="_blank" rel="noopener">Changelog</a>
        <br/>Released under the MIT or Apache-2.0 license.
      `,
      copyright: "Copyright &copy; 2025-2026 TalaDB",
    },

    editLink: {
      pattern: "https://github.com/taladb/taladb/edit/main/docs/:path",
      text: "Edit this page on GitHub",
    },

    search: {
      provider: "local",
    },

    outline: { level: [2, 3], label: "On this page" },
    docFooter: { prev: "Previous", next: "Next" },
    externalLinkIcon: true,
  },

  markdown: {
    theme: {
      light: "github-light",
      dark: "github-dark",
    },
  },
});

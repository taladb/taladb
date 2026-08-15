import { defineConfig, type HeadConfig } from "vitepress";

const title = "TalaDB";
const description =
  "The embedded vector database for on-device AI. Documents + similarity search built in Rust — browser (WASM + OPFS), Node.js, and React Native. Runs where your model runs. No cloud. No compromise.";
const siteUrl = "https://taladb.dev";
const ogImage = `${siteUrl}/tala-db-banner.png`;

export default defineConfig({
  title,
  description,
  base: "/", // update to "/" once taladb.dev is live
  appearance: false,

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
        href: "https://api.fontshare.com/v2/css?f[]=general-sans@500,600,700,800&display=swap",
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
          "local-first database, rust database, wasm database, react native database, embedded database, nosql, offline-first, taladb, vector database, on-device ai",
      },
    ],
  ],

  sitemap: {
    hostname: siteUrl,
  },

  themeConfig: {
    logo: { src: "/logo.svg", alt: "TalaDB" },
    siteTitle: false,

    nav: [
      { text: "Docs", link: "/introduction" },
      { text: "Guides", link: "/guide/web" },
      { text: "API", link: "/api/collection" },
      { text: "Roadmap", link: "/roadmap" },
      {
        text: "Live Demo",
        items: [
          { text: "Web Demo", link: "https://demo-web.taladb.dev/" },
          { text: "Mobile Demo", link: "https://appetize.io/app/b_ugmjhjghdkgnjux4lzkepvsfma" },
        ],
      },
      {
        text: "v0.11.0",
        items: [
          {
            text: "Changelog",
            link: "https://github.com/taladb/taladb/releases",
          },
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

    sidebar: [
      {
        text: "Getting Started",
        items: [
          { text: "Introduction", link: "/introduction" },
          { text: "Core Concepts", link: "/concepts" },
          { text: "Features", link: "/features" },
          { text: "Benchmarks", link: "/benchmarks" },
        ],
      },
      {
        text: "Platform Guides",
        items: [
          { text: "Web (Browser / WASM)", link: "/guide/web" },
          { text: "Node.js", link: "/guide/node" },
          { text: "React Native", link: "/guide/react-native" },
          { text: "CLI Dev Tools", link: "/guide/cli" },
        ],
      },
      {
        text: "Packages",
        items: [
          { text: "React Hooks (@taladb/react)", link: "/guide/react" },
          { text: "Local-First Data (@taladb/react/query)", link: "/guide/query" },
        ],
      },
      {
        text: "API Reference",
        items: [
          { text: "Vector Search", link: "/api/vector-search" },
          { text: "Search (FTS & Hybrid)", link: "/api/search" },
          { text: "Collection", link: "/api/collection" },
          { text: "Schema Validation", link: "/api/schema" },
          { text: "Filters", link: "/api/filters" },
          { text: "Aggregation", link: "/api/aggregation" },
          { text: "Updates", link: "/api/updates" },
          { text: "Migrations", link: "/api/migrations" },
          { text: "Encryption", link: "/api/encryption" },
          { text: "Live Queries", link: "/api/live-queries" },
          { text: "Change Webhook", link: "/api/webhook" },
        ],
      },
    ],

    socialLinks: [
      { icon: "github", link: "https://github.com/taladb/taladb" },
      { icon: "heart", link: "https://github.com/sponsors/taladb" },
    ],

    footer: {
      message: `
        <a href="/introduction">Docs</a> ·
        <a href="https://www.npmjs.com/package/taladb" target="_blank" rel="noopener">npm</a> ·
        <a href="/roadmap">Roadmap</a> ·
        <a href="https://github.com/taladb/taladb/discussions" target="_blank" rel="noopener">Discussions</a> ·
        <a href="https://github.com/taladb/taladb/releases" target="_blank" rel="noopener">Changelog</a> ·
        <a href="https://github.com/sponsors/taladb" target="_blank" rel="noopener">Sponsor</a>
        <br/>Released under the Apache License 2.0.
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
  },

  markdown: {
    theme: {
      light: "github-light",
      dark: "github-dark",
    },
  },
});

<template>
  <section class="home-hero">
    <div class="hero-container">
      <!-- Left: headline + CTAs -->
      <div class="hero-left">
        <div class="hero-eyebrow">
          <span class="badge badge-rust">Built in Rust</span>
          <span class="badge badge-open">Open Source</span>
        </div>

        <h1 class="hero-headline">
          The embedded vector database<br />
          for <span class="hero-accent">on-device</span> AI
        </h1>

        <p class="hero-sub">
          Your model already runs on the device. Your vector store probably
          doesn't — so every query still pays for a network round-trip. TalaDB
          puts documents, similarity search, and sync in one embedded database
          built in Rust, running on device across browser, Node.js, and React Native.
        </p>

        <div class="hero-actions">
          <a href="/introduction" class="btn btn-primary">Get Started</a>
          <a href="https://demo-web.taladb.dev/" class="btn btn-secondary" target="_blank" rel="noopener">
            Web Demo →
          </a>
          <a href="https://appetize.io/app/b_ugmjhjghdkgnjux4lzkepvsfma" class="btn btn-secondary" target="_blank" rel="noopener">
            Mobile Demo →
          </a>
          <a href="https://github.com/taladb/taladb" class="btn btn-ghost" target="_blank" rel="noopener">
            <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
              <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0 0 16 8c0-4.42-3.58-8-8-8z"/>
            </svg>
            GitHub
          </a>
        </div>

        <p class="hero-install">
          <code>pnpm add taladb @taladb/web</code>
        </p>
      </div>

      <!-- Right: code window -->
      <div class="hero-right">
        <div class="code-window">
          <div class="code-window-header">
            <span class="dot dot-red"></span>
            <span class="dot dot-yellow"></span>
            <span class="dot dot-green"></span>
            <span class="code-window-title">app.ts</span>
          </div>
          <pre class="code-window-body"><code><span class="token-keyword">import</span> <span class="token-punctuation">{</span> openDB <span class="token-punctuation">}</span> <span class="token-keyword">from</span> <span class="token-string">'taladb'</span>
<span class="token-keyword">import</span> <span class="token-punctuation">{</span> pipeline <span class="token-punctuation">}</span> <span class="token-keyword">from</span> <span class="token-string">'@xenova/transformers'</span>

<span class="token-comment">// Open a persistent on-device database</span>
<span class="token-keyword">const</span> db <span class="token-operator">=</span> <span class="token-keyword">await</span> <span class="token-function">openDB</span><span class="token-punctuation">(</span><span class="token-string">'myapp.db'</span><span class="token-punctuation">)</span>
<span class="token-keyword">const</span> articles <span class="token-operator">=</span> db<span class="token-punctuation">.</span><span class="token-function">collection</span><span class="token-punctuation">&lt;</span>Article<span class="token-punctuation">&gt;(</span><span class="token-string">'articles'</span><span class="token-punctuation">)</span>

<span class="token-comment">// On-device embedding model — no API key</span>
<span class="token-keyword">const</span> embed <span class="token-operator">=</span> <span class="token-keyword">await</span> <span class="token-function">pipeline</span><span class="token-punctuation">(</span><span class="token-string">'feature-extraction'</span><span class="token-punctuation">,</span>
  <span class="token-string">'Xenova/all-MiniLM-L6-v2'</span><span class="token-punctuation">)</span>

<span class="token-comment">// Store a document with its vector embedding</span>
<span class="token-keyword">await</span> articles<span class="token-punctuation">.</span><span class="token-function">createVectorIndex</span><span class="token-punctuation">(</span><span class="token-string">'embedding'</span><span class="token-punctuation">,</span> <span class="token-punctuation">{</span> dimensions<span class="token-punctuation">:</span> <span class="token-number">384</span> <span class="token-punctuation">}</span><span class="token-punctuation">)</span>
<span class="token-keyword">await</span> articles<span class="token-punctuation">.</span><span class="token-function">insert</span><span class="token-punctuation">(</span><span class="token-punctuation">{</span>
  title<span class="token-punctuation">:</span> <span class="token-string">'Getting started with TalaDB'</span><span class="token-punctuation">,</span>
  embedding<span class="token-punctuation">:</span> <span class="token-keyword">await</span> <span class="token-function">embed</span><span class="token-punctuation">(</span>article<span class="token-punctuation">.</span>body<span class="token-punctuation">)</span><span class="token-punctuation">,</span>
<span class="token-punctuation">})</span>

<span class="token-comment">// Filtered vector search — narrow, then rank</span>
<span class="token-keyword">const</span> results <span class="token-operator">=</span> <span class="token-keyword">await</span> articles<span class="token-punctuation">.</span><span class="token-function">findNearest</span><span class="token-punctuation">(</span>
  <span class="token-string">'embedding'</span><span class="token-punctuation">,</span>
  <span class="token-keyword">await</span> <span class="token-function">embed</span><span class="token-punctuation">(</span><span class="token-string">'how do I reset my password?'</span><span class="token-punctuation">)</span><span class="token-punctuation">,</span>
  <span class="token-number">5</span><span class="token-punctuation">,</span>
  <span class="token-punctuation">{</span> locale<span class="token-punctuation">:</span> <span class="token-string">'en'</span><span class="token-punctuation">,</span> published<span class="token-punctuation">:</span> <span class="token-keyword">true</span> <span class="token-punctuation">}</span>
<span class="token-punctuation">)</span>
<span class="token-comment">// [{ document: Article, score: 0.94 }, ...]</span>
</code></pre>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
// No reactive state needed — purely presentational
</script>

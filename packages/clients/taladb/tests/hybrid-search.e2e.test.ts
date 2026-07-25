// Text + hybrid search e2e against the real native engine — exercises the
// full path: TS adapter → @taladb/node → taladb-core. Skipped when the native
// module isn't built.
import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { openDB } from '../src/index';

let nativeAvailable = true;
try {
  await import('@taladb/node');
} catch {
  nativeAvailable = false;
}

interface Article {
  _id?: string;
  title: string;
  body: string;
  locale: string;
  embedding?: number[];
  [key: string]: unknown;
}

// Crude, inspectable 4-dim "embeddings" — TOPIC_A vs TOPIC_B are orthogonal.
const TOPIC_A = [1, 0, 0, 0];
const TOPIC_B = [0, 1, 0, 0];

describe.skipIf(!nativeAvailable)('text + hybrid search (native engine)', () => {
  let dir: string;
  let db: Awaited<ReturnType<typeof openDB>>;

  beforeAll(async () => {
    dir = mkdtempSync(join(tmpdir(), 'taladb-hybrid-'));
    db = await openDB(join(dir, 'test.db'));
    const articles = db.collection<Article>('articles');

    await articles.insertMany([
      { title: 'reset', body: 'how to reset your password', locale: 'en', embedding: TOPIC_A },
      { title: 'recover', body: 'account recovery and login help', locale: 'en', embedding: TOPIC_A },
      { title: 'sku', body: 'order SKU-9931 has shipped', locale: 'en', embedding: TOPIC_B },
      { title: 'billing', body: 'update your billing details', locale: 'tl', embedding: TOPIC_B },
    ]);

    await articles.createFtsIndex('body');
    await articles.createVectorIndex('embedding', { dimensions: 4 });
  });

  afterAll(async () => {
    await db.close?.();
    rmSync(dir, { recursive: true, force: true });
  });

  it('createFtsIndex works on the native engine', async () => {
    // Regression guard: this method was entirely missing from the napi
    // binding before 0.10, so it threw TypeError on Node.
    const { fts } = await db.collection<Article>('articles').listIndexes();
    expect(fts).toContain('body');
  });

  it('searchText ranks by BM25 relevance', async () => {
    const hits = await db
      .collection<Article>('articles')
      .searchText('body', 'reset password', 5);
    expect(hits.length).toBeGreaterThan(0);
    expect(hits[0].document.title).toBe('reset');
    expect(hits[0].score).toBeGreaterThan(0);
  });

  it('searchText honours a metadata filter', async () => {
    const hits = await db
      .collection<Article>('articles')
      .searchText('body', 'your', 10, { locale: 'en' });
    expect(hits.every((h) => h.document.locale === 'en')).toBe(true);
  });

  it('hybridSearch recovers an exact term the vector side would miss', async () => {
    // Query vector points at TOPIC_A, but the exact SKU (TOPIC_B) should win.
    const hits = await db.collection<Article>('articles').hybridSearch(
      { textField: 'body', text: 'SKU-9931' },
      { vectorField: 'embedding', vector: TOPIC_A },
      5,
    );
    expect(hits[0].document.title).toBe('sku');
  });

  it('hybridSearch reports per-retriever ranks', async () => {
    const hits = await db.collection<Article>('articles').hybridSearch(
      { textField: 'body', text: 'reset password' },
      { vectorField: 'embedding', vector: TOPIC_A },
      5,
    );
    const reset = hits.find((h) => h.document.title === 'reset');
    expect(reset).toBeDefined();
    // Found by BM25 (exact terms) and by the vector index (TOPIC_A).
    expect(reset!.textRank).not.toBeNull();
    expect(reset!.vectorRank).not.toBeNull();
  });

  it('hybridSearch weights can disable the text side', async () => {
    const hits = await db.collection<Article>('articles').hybridSearch(
      { textField: 'body', text: 'SKU-9931' },
      { vectorField: 'embedding', vector: TOPIC_A },
      5,
      undefined,
      { textWeight: 0, vectorWeight: 1 },
    );
    // With text disabled, a TOPIC_A vector must rank a TOPIC_A doc first,
    // not the SKU that only the text side found.
    expect(['reset', 'recover']).toContain(hits[0].document.title);
  });

  it('hybridSearch applies the filter to both retrievers', async () => {
    const hits = await db.collection<Article>('articles').hybridSearch(
      { textField: 'body', text: 'your details' },
      { vectorField: 'embedding', vector: TOPIC_B },
      10,
      { locale: 'en' },
    );
    expect(hits.every((h) => h.document.locale === 'en')).toBe(true);
  });
});

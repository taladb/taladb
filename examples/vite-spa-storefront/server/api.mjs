/**
 * A mock product API — an ordinary paged REST endpoint, the kind a team already
 * has on Express + Postgres. Nothing here knows what TalaDB is.
 *
 * It supports the paging/filter/sort parameters used by the local-first query
 * example. Snapshot/delta parameters remain available to make the mock useful
 * for experiments, but the client does not depend on a replication protocol.
 *
 * GET /api/products?page=&limit=&category=&sort=&order=
 */
import { createServer } from 'node:http';

const PORT = 8787;
const CATEGORIES = ['kitchen', 'garden', 'office', 'outdoor', 'lighting'];

// ---------------------------------------------------------------------------
// The "database": 5,000 products, each with a monotonic revision.
// ---------------------------------------------------------------------------
let revision = 0;
const products = new Map();

for (let i = 0; i < 5000; i++) {
  const id = `sku-${String(i).padStart(5, '0')}`;
  products.set(id, {
    id,
    name: `Product ${i}`,
    price: 50 + ((i * 37) % 4950),
    category: CATEGORIES[i % CATEGORIES.length],
    thumb: `/img/${i % 20}.jpg`,
    rev: ++revision,
    deleted: false,
  });
}

/** Mutate a random product every few seconds, so the delta feed has something to say. */
setInterval(() => {
  const ids = [...products.keys()];
  const id = ids[Math.floor(Math.random() * ids.length)];
  const row = products.get(id);
  row.price = 50 + Math.floor(Math.random() * 4950);
  row.rev = ++revision;
  console.log(`[origin] repriced ${id} → ₱${row.price} (rev ${row.rev})`);
}, 5000);

const live = () => [...products.values()].filter((p) => !p.deleted);
const publicFields = ({ id, name, price, category, thumb, rev }) => ({
  id,
  name,
  price,
  category,
  thumb,
  rev,
});

// ---------------------------------------------------------------------------
let requestCount = 0;

const server = createServer((req, res) => {
  const url = new URL(req.url, `http://localhost:${PORT}`);
  res.setHeader('access-control-allow-origin', '*');
  res.setHeader('content-type', 'application/json');

  if (url.pathname === '/api/requests') {
    // The demo's scoreboard. Not part of the replication contract.
    return res.end(JSON.stringify({ count: requestCount }));
  }
  if (url.pathname === '/api/requests/reset') {
    requestCount = 0;
    return res.end(JSON.stringify({ count: 0 }));
  }
  if (url.pathname !== '/api/products') {
    res.statusCode = 404;
    return res.end(JSON.stringify({ error: 'not found' }));
  }

  requestCount++;
  const since = url.searchParams.get('since');

  // --- Delta feed -----------------------------------------------------------
  if (since !== null) {
    const from = Number(since);
    const all = [...products.values()].filter((p) => p.rev > from);
    const body = {
      data: all.filter((p) => !p.deleted).map(publicFields),
      // Deletions must be reported explicitly. A paged GET only returns
      // survivors, and a row's *absence* is ambiguous — deleted, or shifted to
      // another page? The client refuses to guess, so if we don't say so here,
      // deleted rows live on in every client forever.
      deleted: all.filter((p) => p.deleted).map((p) => p.id),
      cursor: String(revision),
      hasMore: false,
    };
    console.log(
      `[origin] delta since=${from} → ${body.data.length} changed, ${body.deleted.length} deleted`,
    );
    return res.end(JSON.stringify(body));
  }

  // --- Bootstrap walk -------------------------------------------------------
  const limit = Math.min(Number(url.searchParams.get('limit') ?? 500), 1000);
  const offset = Number(url.searchParams.get('page') ?? 0);

  // Issue a snapshot on the first page; honour it on every later one. Every page
  // of this walk therefore reads the same logical view of the table.
  const snapshot = url.searchParams.get('snapshot') ?? String(revision);
  let view = live()
    .filter((p) => p.rev <= Number(snapshot))
    .sort((a, b) => a.id.localeCompare(b.id));

  const category = url.searchParams.get('category');
  if (category) view = view.filter((p) => p.category === category);
  const sort = url.searchParams.get('sort');
  const order = url.searchParams.get('order') === 'desc' ? -1 : 1;
  if (sort === 'price') view.sort((a, b) => (a.price - b.price) * order);

  const slice = view.slice(offset, offset + limit);
  const nextPage = offset + limit >= view.length ? null : offset + limit;

  console.log(
    `[origin] bootstrap page offset=${offset} limit=${limit} snapshot=${snapshot} → ${slice.length} rows`,
  );

  res.end(
    JSON.stringify({
      data: slice.map(publicFields),
      nextPage,
      snapshot,
      // Issued as of the snapshot, on the first page — not after the walk. Any
      // change made *during* the walk would otherwise fall between "bootstrap
      // finished" and "delta started", and be lost forever.
      deltaCursor: snapshot,
      total: view.length,
    }),
  );
});

server.listen(PORT, () => {
  console.log(`[origin] mock product API on http://localhost:${PORT}`);
  console.log(`[origin] ${products.size} products, snapshot-consistent paging, delta feed`);
});

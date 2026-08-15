/** Durable query caching in a Vite SPA, backed by a real local TalaDB collection. */
import { useEffect, useState } from 'react'
import { TalaDBProvider } from '@taladb/react'
import { QueryProvider, useQuery } from '@taladb/react/query'
import { deriveDocId, type Document } from 'taladb'

interface Product extends Document {
  _id: string
  sku: string
  name: string
  price: number
  category: string
  thumb: string
}

interface ProductRow {
  id: string
  name: string
  price: number
  category: string
  thumb: string
}

const API = 'http://localhost:8787'
const CATEGORIES = ['all', 'kitchen', 'garden', 'office', 'outdoor', 'lighting']
const PAGE_SIZE = 24

function RequestCounter() {
  const [count, setCount] = useState(0)
  useEffect(() => {
    const tick = () =>
      fetch(`${API}/api/requests`)
        .then((response) => response.json())
        .then((body: { count: number }) => setCount(body.count))
        .catch(() => {})
    tick()
    const id = setInterval(tick, 500)
    return () => clearInterval(id)
  }, [])

  return (
    <div className="counter">
      <span className="counter-num">{count}</span>
      <span className="counter-label">product requests to the origin</span>
    </div>
  )
}

function Catalog() {
  const [page, setPage] = useState(1)
  const [category, setCategory] = useState('all')
  const [sort, setSort] = useState<1 | -1>(1)

  const { data, loading, stale, error } = useQuery<Product>({
    collection: 'products',
    key: ['products', { page, category, sort }],
    ttl: 5 * 60_000,
    fetch: async ({ signal }) => {
      const params = new URLSearchParams({
        page: String((page - 1) * PAGE_SIZE),
        limit: String(PAGE_SIZE),
        sort: 'price',
        order: sort === -1 ? 'desc' : 'asc',
      })
      if (category !== 'all') params.set('category', category)
      const response = await fetch(`${API}/api/products?${params}`, { signal })
      if (!response.ok) throw new Error(`Origin returned ${response.status}`)
      const body = (await response.json()) as { data: ProductRow[] }
      return body.data.map((row) => ({
        _id: deriveDocId('products', row.id),
        sku: row.id,
        name: row.name,
        price: row.price,
        category: row.category,
        thumb: row.thumb,
      }))
    },
  })

  return (
    <>
      <header>
        <div>
          <h1>Storefront</h1>
          <p className="sub">
            {loading
              ? 'Opening this page…'
              : stale
                ? 'Showing local data while refreshing…'
                : '✓ Served from TalaDB; cached pages work offline'}
          </p>
        </div>
        <RequestCounter />
      </header>

      <div className="controls">
        <div className="chips">
          {CATEGORIES.map((value) => (
            <button
              key={value}
              className={value === category ? 'chip on' : 'chip'}
              onClick={() => {
                setCategory(value)
                setPage(1)
              }}
            >
              {value}
            </button>
          ))}
        </div>
        <button className="chip" onClick={() => setSort((value) => (value === 1 ? -1 : 1))}>
          price {sort === 1 ? '↑' : '↓'}
        </button>
      </div>

      {error && data.length === 0 ? (
        <p className="empty">Could not load this uncached page.</p>
      ) : loading ? (
        <p className="empty">Loading…</p>
      ) : data.length === 0 ? (
        <p className="empty">No products on this page.</p>
      ) : (
        <div className="grid">
          {data.map((product) => (
            <article key={product._id} className="card">
              <div className="thumb" />
              <h3>{product.name}</h3>
              <p className="cat">{product.category}</p>
              <p className="price">₱{product.price.toLocaleString()}</p>
            </article>
          ))}
        </div>
      )}

      <nav className="pager">
        <button disabled={page === 1} onClick={() => setPage((value) => value - 1)}>
          ← prev
        </button>
        <span>page {page}</span>
        <button disabled={data.length < PAGE_SIZE} onClick={() => setPage((value) => value + 1)}>
          next →
        </button>
      </nav>

      <footer>
        Revisit a page within five minutes and the request counter stays still. Turn the network
        off and every page you already opened remains queryable from the device.
      </footer>
    </>
  )
}

export default function App() {
  return (
    <TalaDBProvider name="storefront.db" fallback={<p className="empty">Opening database…</p>}>
      <QueryProvider>
        <Catalog />
      </QueryProvider>
    </TalaDBProvider>
  )
}

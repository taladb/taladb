/**
 * URL templates.
 *
 * The template is what lets routing live at the call site while the write still
 * survives its component: it is stored on the queued document and resolved by
 * the drain, possibly after a reload. So the substitution rules are part of the
 * contract, not an implementation detail.
 */
import { describe, it, expect } from 'vitest'
import { resolveUrl, defaultClassify, DEFAULT_METHODS } from '../../../src/query/endpoint'

const ID = '01ARZ3NDEKTSV4RRFFQ69G5FAV'

describe('resolveUrl', () => {
  it('drops the trailing id segment for an insert', () => {
    // One template covers all three verbs, so a POST does not need its own.
    expect(resolveUrl('/api/todos/:id', 'todos', ID, 'insert')).toBe('/api/todos')
  })

  it('substitutes the id for an update or delete', () => {
    expect(resolveUrl('/api/todos/:id', 'todos', ID, 'update')).toBe(`/api/todos/${ID}`)
    expect(resolveUrl('/api/todos/:id', 'todos', ID, 'delete')).toBe(`/api/todos/${ID}`)
  })

  it('substitutes the collection name', () => {
    expect(resolveUrl('/api/v2/:collection/:id', 'todos', ID, 'update')).toBe(
      `/api/v2/todos/${ID}`,
    )
  })

  it('keeps an embedded id on an insert', () => {
    // Only a *trailing* id segment is the record being created.
    expect(resolveUrl('/api/:id/todos', 'todos', ID, 'insert')).toBe(`/api/${ID}/todos`)
  })

  it('tolerates a trailing slash', () => {
    expect(resolveUrl('/api/todos/:id/', 'todos', ID, 'insert')).toBe('/api/todos')
  })

  it('leaves a template without placeholders alone', () => {
    expect(resolveUrl('/api/todos', 'todos', ID, 'update')).toBe('/api/todos')
  })

  it('works with an absolute url', () => {
    expect(resolveUrl('https://api.example.com/todos/:id', 'todos', ID, 'update')).toBe(
      `https://api.example.com/todos/${ID}`,
    )
  })
})

describe('defaultClassify', () => {
  const res = (status: number) => new Response(null, { status })

  it('accepts a 2xx', () => {
    expect(defaultClassify(res(200))).toBe('ok')
    expect(defaultClassify(res(201))).toBe('ok')
  })

  it('reads 409 as already applied', () => {
    // The retry-after-timeout case. Guessing wrong here stalls the queue.
    expect(defaultClassify(res(409))).toBe('applied')
  })

  it('retries what may recover', () => {
    expect(defaultClassify(res(500))).toBe('retry')
    expect(defaultClassify(res(503))).toBe('retry')
    expect(defaultClassify(res(408))).toBe('retry')
    expect(defaultClassify(res(429))).toBe('retry')
  })

  it('parks an ordinary client error', () => {
    expect(defaultClassify(res(400))).toBe('terminal')
    expect(defaultClassify(res(401))).toBe('terminal')
    expect(defaultClassify(res(422))).toBe('terminal')
  })
})

describe('DEFAULT_METHODS', () => {
  it('matches the change webhook, so one receiver serves both', () => {
    expect(DEFAULT_METHODS).toEqual({ insert: 'POST', update: 'PUT', delete: 'DELETE' })
  })
})

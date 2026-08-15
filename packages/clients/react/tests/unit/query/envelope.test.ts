/**
 * Envelope stamping and write coalescing.
 *
 * Coalescing is where the design either stays small or grows a replay log, so
 * each transition is pinned here rather than left to the drain loop to imply.
 */
import { describe, it, expect } from 'vitest'
import {
  ENVELOPE_FIELDS,
  reservedFieldsIn,
  stampSynced,
  stampPending,
  coalesce,
} from '../../../src/query/envelope'

describe('reservedFieldsIn', () => {
  it('finds an envelope field smuggled in by a server response', () => {
    // The real hazard: a response carrying `_sync` overwrites the envelope and
    // silently drops the document out of the pending queue.
    expect(reservedFieldsIn({ title: 'x', _sync: 'synced' })).toEqual(['_sync'])
  })

  it('reports every collision, not just the first', () => {
    expect(reservedFieldsIn({ _sync: 1, _attempt: 2, _error: 3 })).toEqual([
      '_sync',
      '_attempt',
      '_error',
    ])
  })

  it('passes an ordinary document', () => {
    expect(reservedFieldsIn({ _id: 'x', title: 'y', done: false })).toEqual([])
  })

  it('does not claim the engine\'s own fields', () => {
    // `_id`, `_changed_at` and `_v` belong to the engine, not to this layer.
    expect(reservedFieldsIn({ _id: 'a', _changed_at: 1, _v: 1 })).toEqual([])
  })

  it('detects a field set to undefined, which still overwrites on spread', () => {
    expect(reservedFieldsIn({ _sync: undefined })).toEqual(['_sync'])
  })
})

describe('stampSynced', () => {
  it('marks the document clean and records when the server spoke', () => {
    expect(stampSynced({ _id: 'a', title: 'x' }, 1000)).toEqual({
      _id: 'a',
      title: 'x',
      _sync: 'synced',
      _attempt: 0,
      _error: null,
      _fetched_at: 1000,
      _retry_at: 0,
      _revision: null,
    })
  })

  it('clears a previous failure', () => {
    const failed = { _id: 'a', _sync: 'failed', _attempt: 3, _error: 'boom' }
    const synced = stampSynced(failed, 5)
    expect(synced._error).toBeNull()
    expect(synced._attempt).toBe(0)
    expect(synced._sync).toBe('synced')
  })
})

describe('stampPending', () => {
  it('queues the document without inventing a hydration time', () => {
    const doc = stampPending({ _id: 'a', title: 'x' }, 'insert')
    expect(doc._sync).toBe('pending')
    expect(doc._op).toBe('insert')
    expect(doc._fetched_at).toBe(0)
    expect(typeof doc._revision).toBe('string')
  })

  it('preserves when the server last spoke about this document', () => {
    // A local edit does not change when the server was last heard from.
    const doc = stampPending({ _id: 'a' }, 'update', { _fetched_at: 900 })
    expect(doc._fetched_at).toBe(900)
  })

  it('resets the retry counter for the new attempt', () => {
    const doc = stampPending({ _id: 'a' }, 'update', { _fetched_at: 1, _attempt: 4 } as never)
    expect(doc._attempt).toBe(0)
    expect(doc._error).toBeNull()
    expect(doc._retry_at).toBe(0)
  })
})

describe('coalesce', () => {
  it('queues a write against a clean document', () => {
    expect(coalesce(undefined, 'insert')).toBe('insert')
    expect(coalesce(undefined, 'update')).toBe('update')
    expect(coalesce(undefined, 'delete')).toBe('delete')
  })

  it('keeps an unsent insert an insert when edited', () => {
    // The server has never seen this document, so it only needs the final shape.
    expect(coalesce('insert', 'update')).toBe('insert')
  })

  it('cancels an unsent insert that is then deleted', () => {
    // Nothing to tell the server about — it never knew.
    expect(coalesce('insert', 'delete')).toBeNull()
  })

  it('merges repeated updates into one', () => {
    expect(coalesce('update', 'update')).toBe('update')
  })

  it('lets a delete supersede a pending update', () => {
    expect(coalesce('update', 'delete')).toBe('delete')
  })

  it('treats a pending delete as terminal', () => {
    expect(coalesce('delete', 'update')).toBe('delete')
    expect(coalesce('delete', 'delete')).toBe('delete')
  })
})

describe('ENVELOPE_FIELDS', () => {
  it('is exactly the set the reserved-name check enforces', () => {
    expect([...ENVELOPE_FIELDS]).toEqual([
      '_sync',
      '_op',
      '_attempt',
      '_error',
      '_fetched_at',
      '_retry_at',
      '_endpoint',
      '_method',
      '_revision',
    ])
  })

  it('no longer carries _expires_at', () => {
    // TTL drives revalidation, never deletion: these documents live in the
    // application's own collections, so a sweep would delete real user data.
    expect([...ENVELOPE_FIELDS]).not.toContain('_expires_at')
  })
})

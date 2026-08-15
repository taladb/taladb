/**
 * The backend-contract validator.
 *
 * Each rule exists because its violation is silent and delayed — a duplicated
 * row, a local copy that drifts, a queue that stalls. The tests pin that the
 * warning fires on the violation and stays quiet otherwise, since a validator
 * that cries wolf gets muted and then the real warning is missed too.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import {
  checkResponseId,
  checkResponseBody,
  checkClassification,
  checkMutationFilter,
} from '../../../src/query/validate'
import type { PendingWrite } from '../../../src/query/types'

const op: PendingWrite = {
  collection: 'todos',
  id: '01ARZ3NDEKTSV4RRFFQ69G5FAV',
  type: 'insert',
  document: { title: 'x' },
}

let warn: ReturnType<typeof vi.spyOn>
beforeEach(() => {
  warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
})
afterEach(() => warn.mockRestore())

describe('rule 1 / 2 — client ids are authoritative and writes are idempotent', () => {
  it('warns that every write will duplicate when the id changes on a first attempt', () => {
    checkResponseId(op, { _id: 'server-made-this-up' }, 0)
    expect(warn.mock.calls[0][0]).toMatch(/rule 1/)
    expect(warn.mock.calls[0][0]).toMatch(/duplicate/)
  })

  it('warns that this write just duplicated when the id changes on a retry', () => {
    // Different consequence, same detection — worth saying precisely.
    checkResponseId(op, { _id: 'a-second-row' }, 2)
    expect(warn.mock.calls[0][0]).toMatch(/rule 2/)
    expect(warn.mock.calls[0][0]).toMatch(/just been duplicated/)
  })

  it('stays quiet when the server echoes the id it was sent', () => {
    checkResponseId(op, { _id: op.id, title: 'x' }, 0)
    expect(warn).not.toHaveBeenCalled()
  })

  it('stays quiet when the body carries no id at all', () => {
    // That is rule 4's business, not this one — one warning per problem.
    checkResponseId(op, { title: 'x' }, 0)
    expect(warn).not.toHaveBeenCalled()
  })
})

describe('rule 4 — the response body is canonical', () => {
  it('warns when a successful write answers with no document', () => {
    checkResponseBody(op, null)
    expect(warn.mock.calls[0][0]).toMatch(/rule 4/)
  })

  it('does not ask a delete for a body', () => {
    checkResponseBody({ ...op, type: 'delete', document: null }, null)
    expect(warn).not.toHaveBeenCalled()
  })

  it('stays quiet when a document comes back', () => {
    checkResponseBody(op, { _id: op.id })
    expect(warn).not.toHaveBeenCalled()
  })
})

describe('rule 3 — error classes are explicit', () => {
  it('warns when a 4xx is marked retryable, because it will never succeed', () => {
    checkClassification(op, 422, 'retry')
    expect(warn.mock.calls[0][0]).toMatch(/rule 3/)
    expect(warn.mock.calls[0][0]).toMatch(/until the tab closes/)
  })

  it('allows 408 and 429 to be retried', () => {
    // Both are 4xx that explicitly mean "try again".
    checkClassification(op, 408, 'retry')
    checkClassification(op, 429, 'retry')
    expect(warn).not.toHaveBeenCalled()
  })

  it('warns when a 5xx is parked as terminal', () => {
    checkClassification(op, 503, 'terminal')
    expect(warn.mock.calls[0][0]).toMatch(/may recover/)
  })

  it('stays quiet on the ordinary mappings', () => {
    checkClassification(op, 200, 'ok')
    checkClassification(op, 409, 'applied')
    checkClassification(op, 503, 'retry')
    checkClassification(op, 422, 'terminal')
    expect(warn).not.toHaveBeenCalled()
  })
})

describe('rule 6 — mutation filters must be _id-based', () => {
  it('warns on a field filter', () => {
    checkMutationFilter({ done: false })
    expect(warn.mock.calls[0][0]).toMatch(/must be \{ _id \}/)
  })

  it('warns on a filter that merely includes _id', () => {
    // The primary re-evaluates the whole filter, so the extra field still bites.
    checkMutationFilter({ _id: 'x', done: false })
    expect(warn).toHaveBeenCalled()
  })

  it('stays quiet on an _id filter', () => {
    checkMutationFilter({ _id: 'x' })
    expect(warn).not.toHaveBeenCalled()
  })
})

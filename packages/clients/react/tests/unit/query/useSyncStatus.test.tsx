/**
 * useSyncStatus — making silent failure visible.
 *
 * Without this hook an optimistic write that fails after the user navigated
 * away disappears with no symptom. Everything here is about that: the count is
 * live, failures carry enough to explain themselves, and both ways out of a
 * failure actually change the stored state.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { renderHook, waitFor, act } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useMutation } from '../../../src/query/useMutation'
import { useSyncStatus } from '../../../src/query/useSyncStatus'
import { createFakeDB } from '../../helpers/fakeQueryDB'

interface Todo extends Document {
  _id?: string
  title: string
}

function setup() {
  const { db, docsIn } = createFakeDB()
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider drain={{ policy: 'manual' }}>{children}</QueryProvider>
    </TalaDBProvider>
  )
  return { db, wrapper, docsIn }
}

/** Render both hooks together — the status hook only sees registered collections. */
function useBoth() {
  const mutation = useMutation<Todo>({ collection: 'todos' })
  const status = useSyncStatus()
  return { mutation, status }
}

describe('useSyncStatus', () => {
  it('counts a queued write as pending, live', async () => {
    const { wrapper } = setup()
    const { result } = renderHook(useBoth, { wrapper })

    await waitFor(() => expect(result.current.status.pending).toBe(0))

    await act(async () => {
      await result.current.mutation.mutateAsync({ type: 'insert', doc: { title: 'x' } })
    })

    await waitFor(() => expect(result.current.status.pending).toBe(1))
  })

  it('reports a terminal failure with enough to explain it', async () => {
    const { wrapper, db } = setup()
    const { result } = renderHook(useBoth, { wrapper })

    await act(async () => {
      await result.current.mutation.mutateAsync({ type: 'insert', doc: { title: 'x' } })
    })
    await waitFor(() => expect(result.current.status.pending).toBe(1))

    // Stand in for the drain parking it after a 422.
    const todos = db.collection<Document>('todos')
    const id = (await todos.find())[0]._id as string
    await act(async () => {
      await todos.updateOne(
        { _id: id },
        { $set: { _sync: 'failed', _error: 'The server rejected this insert with status 422.' } },
      )
    })

    await waitFor(() => expect(result.current.status.failed).toHaveLength(1))
    const [failure] = result.current.status.failed
    expect(failure.collection).toBe('todos')
    expect(failure.op).toBe('insert')
    expect(failure.error).toMatch(/422/)
    // A failure is no longer pending — it will not be sent again unaided.
    expect(result.current.status.pending).toBe(0)
  })

  it('retry() puts a failed write back in the queue', async () => {
    const { wrapper, db } = setup()
    const { result } = renderHook(useBoth, { wrapper })

    await act(async () => {
      await result.current.mutation.mutateAsync({ type: 'insert', doc: { title: 'x' } })
    })
    const todos = db.collection<Document>('todos')
    const id = (await todos.find())[0]._id as string
    await act(async () => {
      await todos.updateOne({ _id: id }, { $set: { _sync: 'failed', _attempt: 3, _error: 'no' } })
    })
    await waitFor(() => expect(result.current.status.failed).toHaveLength(1))

    await act(async () => {
      result.current.status.retry(id)
    })

    await waitFor(() => expect(result.current.status.pending).toBe(1))
    const doc = await todos.findOne({ _id: id })
    expect(doc?._sync).toBe('pending')
    expect(doc?._attempt).toBe(0) // a fresh start, not a continuation
    expect(doc?._retry_at).toBe(0) // and eligible immediately
  })

  it('discard() removes an insert the server never accepted', async () => {
    const { wrapper, db } = setup()
    const { result } = renderHook(useBoth, { wrapper })

    await act(async () => {
      await result.current.mutation.mutateAsync({ type: 'insert', doc: { title: 'x' } })
    })
    const todos = db.collection<Document>('todos')
    const id = (await todos.find())[0]._id as string
    await act(async () => {
      await todos.updateOne({ _id: id }, { $set: { _sync: 'failed', _error: 'no' } })
    })
    await waitFor(() => expect(result.current.status.failed).toHaveLength(1))

    await act(async () => {
      result.current.status.discard(id)
    })

    // Nothing to keep: the server never knew this document existed.
    await waitFor(async () => expect(await todos.count()).toBe(0))
  })

  it('discard() keeps the document for a failed update', async () => {
    const { wrapper, db } = setup()
    const { result } = renderHook(useBoth, { wrapper })
    const todos = db.collection<Document>('todos')

    await act(async () => {
      await todos.insert({ title: 'kept', _sync: 'failed', _op: 'update', _error: 'no' })
    })
    await waitFor(() => expect(result.current.status.failed).toHaveLength(1))
    const id = (await todos.find())[0]._id as string

    await act(async () => {
      result.current.status.discard(id)
    })

    await waitFor(async () => expect((await todos.findOne({ _id: id }))?._sync).toBe('synced'))
    // The row survives — discarding a failed update stops the retry, it does
    // not delete the user's data.
    expect(await todos.count()).toBe(1)
  })

  it('reports connectivity', async () => {
    const { wrapper } = setup()
    const { result } = renderHook(useBoth, { wrapper })
    await waitFor(() => expect(typeof result.current.status.online).toBe('boolean'))
    expect(result.current.status.draining).toBe(false)
  })
})

/**
 * Unit tests for useMutation — the local write hook.
 *
 * Behaviours under test:
 *   - insert / update / delete reach the collection
 *   - update translates { set } to a $set update
 *   - pending reflects an in-flight write and always clears
 *   - a failing write surfaces on `error` rather than throwing to render
 *   - mutateAsync rejects so a caller can await and handle it
 */
import { describe, it, expect, vi } from 'vitest'
import { renderHook, waitFor, act } from '@testing-library/react'
import type { ReactNode } from 'react'
import { TalaDBProvider } from '../../src/context'
import { useMutation } from '../../src/useMutation'
import { createMockDB } from '../helpers/mockCollection'
import type { TalaDB } from 'taladb'

function makeWrapper(db: TalaDB) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <TalaDBProvider db={db}>{children}</TalaDBProvider>
  }
}

describe('useMutation', () => {
  it('inserts through the collection', async () => {
    const { db, getHandle } = createMockDB()
    const { result } = renderHook(() => useMutation({ collection: 'orders' }), {
      wrapper: makeWrapper(db as unknown as TalaDB),
    })

    await act(async () => {
      await result.current.mutateAsync({ type: 'insert', doc: { sku: 'A1' } })
    })

    expect(getHandle('orders').docs()).toContainEqual(expect.objectContaining({ sku: 'A1' }))
  })

  it('translates { set } into a $set update', async () => {
    const { db, getHandle } = createMockDB()
    const updateOne = vi.spyOn(getHandle('orders').collection, 'updateOne')
    const { result } = renderHook(() => useMutation({ collection: 'orders' }), {
      wrapper: makeWrapper(db as unknown as TalaDB),
    })

    await act(async () => {
      await result.current.mutateAsync({
        type: 'update',
        where: { _id: 'o1' },
        set: { status: 'shipped' },
      })
    })

    expect(updateOne).toHaveBeenCalledWith({ _id: 'o1' }, { $set: { status: 'shipped' } })
  })

  it('deletes through the collection', async () => {
    const { db, getHandle } = createMockDB()
    const deleteOne = vi.spyOn(getHandle('orders').collection, 'deleteOne')
    const { result } = renderHook(() => useMutation({ collection: 'orders' }), {
      wrapper: makeWrapper(db as unknown as TalaDB),
    })

    await act(async () => {
      await result.current.mutateAsync({ type: 'delete', where: { _id: 'o1' } })
    })

    expect(deleteOne).toHaveBeenCalledWith({ _id: 'o1' })
  })

  it('surfaces a write failure on `error` without throwing from mutate', async () => {
    const { db, getHandle } = createMockDB()
    vi.spyOn(getHandle('orders').collection, 'insert').mockRejectedValue(new Error('disk full'))
    const { result } = renderHook(() => useMutation({ collection: 'orders' }), {
      wrapper: makeWrapper(db as unknown as TalaDB),
    })

    // `mutate` is fire-and-forget: it must not produce an unhandled rejection.
    act(() => {
      result.current.mutate({ type: 'insert', doc: { sku: 'A1' } })
    })

    await waitFor(() => {
      expect(result.current.error).toBeInstanceOf(Error)
    })
    expect((result.current.error as Error).message).toBe('disk full')
  })

  it('rejects from mutateAsync so an awaiting caller can handle it', async () => {
    const { db, getHandle } = createMockDB()
    vi.spyOn(getHandle('orders').collection, 'insert').mockRejectedValue(new Error('nope'))
    const { result } = renderHook(() => useMutation({ collection: 'orders' }), {
      wrapper: makeWrapper(db as unknown as TalaDB),
    })

    await act(async () => {
      await expect(
        result.current.mutateAsync({ type: 'insert', doc: { sku: 'A1' } }),
      ).rejects.toThrow('nope')
    })
  })

  it('clears `pending` after a failed write, not just a successful one', async () => {
    const { db, getHandle } = createMockDB()
    vi.spyOn(getHandle('orders').collection, 'insert').mockRejectedValue(new Error('nope'))
    const { result } = renderHook(() => useMutation({ collection: 'orders' }), {
      wrapper: makeWrapper(db as unknown as TalaDB),
    })

    act(() => {
      result.current.mutate({ type: 'insert', doc: { sku: 'A1' } })
    })

    await waitFor(() => {
      expect(result.current.error).not.toBeNull()
    })
    expect(result.current.pending).toBe(false)
  })

  it('resets a previous error on the next successful write', async () => {
    const { db, getHandle } = createMockDB()
    const insert = vi.spyOn(getHandle('orders').collection, 'insert')
    insert.mockRejectedValueOnce(new Error('transient'))

    const { result } = renderHook(() => useMutation({ collection: 'orders' }), {
      wrapper: makeWrapper(db as unknown as TalaDB),
    })

    act(() => {
      result.current.mutate({ type: 'insert', doc: { sku: 'A1' } })
    })
    await waitFor(() => expect(result.current.error).not.toBeNull())

    await act(async () => {
      await result.current.mutateAsync({ type: 'insert', doc: { sku: 'A2' } })
    })
    expect(result.current.error).toBeNull()
  })
})

/**
 * The provider's `collections` registry is what stops the hooks from silently
 * bypassing collection configuration.
 *
 * `useCollection` (and therefore `useFind`/`useQuery`/`useWrite`) used to call
 * `db.collection(name)` with no options, so the `schema` and `syncSchema` an app
 * declared via `db.collection(name, { … })` never applied on the hook path: an
 * invalid document written through `useWrite` was accepted, and no `_v` shape
 * version was stamped on it.
 */
import { describe, it, expect, vi } from 'vitest'
import { renderHook, waitFor } from '@testing-library/react'
import React from 'react'
import type { CollectionOptions, Document } from 'taladb'
import { TalaDBProvider } from '../../src/context'
import { useCollection } from '../../src/useCollection'
import { createMockDB } from '../helpers/mockCollection'

interface Booking extends Document {
  _v?: number
  listingId: string
  guests: number
}

const BookingSchema = {
  parse(doc: unknown): Booking {
    const d = doc as Partial<Booking>
    if (typeof d.listingId !== 'string' || !d.listingId) throw new Error('listingId required')
    if (typeof d.guests !== 'number' || d.guests < 1) throw new Error('guests must be >= 1')
    return d as Booking
  },
}

const bookingOptions: CollectionOptions<Booking> = {
  schema: BookingSchema,
  syncSchema: { version: 3, required: ['listingId'] },
}

function wrapper(
  db: ReturnType<typeof createMockDB>['db'],
  collections?: Record<string, CollectionOptions<never>>,
) {
  return ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db} collections={collections}>
      {children}
    </TalaDBProvider>
  )
}

describe('collection options registry', () => {
  it('opens the collection with the options registered on the provider', () => {
    const { db } = createMockDB()
    const spy = vi.spyOn(db, 'collection')

    renderHook(() => useCollection<Booking>('bookings'), {
      wrapper: wrapper(db, { bookings: bookingOptions } as never),
    })

    expect(spy).toHaveBeenCalledWith('bookings', bookingOptions)
  })

  it('leaves unregistered collections unconfigured', () => {
    const { db } = createMockDB()
    const spy = vi.spyOn(db, 'collection')

    renderHook(() => useCollection('notes'), {
      wrapper: wrapper(db, { bookings: bookingOptions } as never),
    })

    expect(spy).toHaveBeenCalledWith('notes', undefined)
  })

  it('lets per-call options override the registry', () => {
    const { db } = createMockDB()
    const spy = vi.spyOn(db, 'collection')
    const override: CollectionOptions<Booking> = { syncSchema: { version: 9 } }

    renderHook(() => useCollection<Booking>('bookings', override), {
      wrapper: wrapper(db, { bookings: bookingOptions } as never),
    })

    expect(spy).toHaveBeenCalledWith('bookings', override)
  })

  it('keeps the handle stable when `collections` is an inline object', async () => {
    // A fresh `{...}` on every render must NOT produce a new collection handle —
    // that would tear down and re-run every live query underneath it.
    const { db } = createMockDB()
    const seen: unknown[] = []

    const Wrapper = ({ children }: { children: React.ReactNode }) => (
      <TalaDBProvider db={db} collections={{ bookings: { syncSchema: { version: 3 } } }}>
        {children}
      </TalaDBProvider>
    )

    const { rerender, result } = renderHook(
      () => {
        const col = useCollection<Booking>('bookings')
        seen.push(col)
        return col
      },
      { wrapper: Wrapper },
    )

    const first = result.current
    rerender()
    rerender()

    await waitFor(() => expect(seen.length).toBeGreaterThan(1))
    expect(result.current).toBe(first)
  })

  it('still resolves a collection with no registry at all', () => {
    const { db } = createMockDB()
    const spy = vi.spyOn(db, 'collection')
    renderHook(() => useCollection('notes'), { wrapper: wrapper(db) })
    expect(spy).toHaveBeenCalledWith('notes', undefined)
  })
})

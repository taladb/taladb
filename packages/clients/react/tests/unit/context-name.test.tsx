/**
 * Name-based <TalaDBProvider name="..."> — the provider owns the openDB
 * lifecycle: fallback until open, children with a ready db afterwards,
 * close on unmount (including a StrictMode-style cancelled open).
 */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { StrictMode } from 'react'
import { render, screen, waitFor, cleanup } from '@testing-library/react'
import type { TalaDB } from 'taladb'
import { TalaDBProvider, resetSharedDatabases, useTalaDB } from '../../src/context'

const closeSpy = vi.fn()
let resolveOpen: ((db: TalaDB) => void) | undefined
let openCalls: Array<{ name: string; options: unknown }> = []

vi.mock('taladb', () => ({
  openDB: (name: string, options: unknown) => {
    openCalls.push({ name, options })
    return new Promise<TalaDB>((res) => {
      resolveOpen = res
    })
  },
}))

const makeDb = (): TalaDB =>
  ({ close: closeSpy, collection: vi.fn() }) as unknown as TalaDB

function Probe() {
  const db = useTalaDB()
  return <div data-testid="ready">{db ? 'db-ready' : 'no-db'}</div>
}

beforeEach(() => {
  cleanup()
  // Handles are shared per database name across the module, so without this a
  // case inherits the previous one's instance and never calls `openDB`.
  resetSharedDatabases()
  closeSpy.mockClear()
  resolveOpen = undefined
  openCalls = []
})

describe('<TalaDBProvider name="...">', () => {
  it('renders the fallback while opening, then children with a ready db', async () => {
    render(
      <TalaDBProvider name="app.db" fallback={<div data-testid="splash">loading</div>}>
        <Probe />
      </TalaDBProvider>,
    )

    expect(screen.getByTestId('splash')).toBeDefined()
    expect(screen.queryByTestId('ready')).toBeNull()
    // the dynamic import('taladb') resolves on a microtask
    await waitFor(() => expect(openCalls).toEqual([{ name: 'app.db', options: undefined }]))

    resolveOpen!(makeDb())
    await waitFor(() => expect(screen.getByTestId('ready').textContent).toBe('db-ready'))
    expect(screen.queryByTestId('splash')).toBeNull()
  })

  it('forwards options to openDB', async () => {
    const options = { config: { sync: { enabled: false } } }
    render(
      <TalaDBProvider name="app.db" options={options}>
        <Probe />
      </TalaDBProvider>,
    )
    await waitFor(() => expect(openCalls[0]).toEqual({ name: 'app.db', options }))
  })

  it('closes the db on unmount', async () => {
    const { unmount } = render(
      <TalaDBProvider name="app.db">
        <Probe />
      </TalaDBProvider>,
    )
    await waitFor(() => expect(resolveOpen).toBeDefined())
    resolveOpen!(makeDb())
    await waitFor(() => expect(screen.getByTestId('ready')).toBeDefined())

    unmount()
    // Deferred, not synchronous: the handle is shared, and a StrictMode
    // remount lands in the same tick as the unmount.
    await waitFor(() => expect(closeSpy).toHaveBeenCalledTimes(1))
  })

  it('closes an orphaned handle when the open resolves after unmount', async () => {
    const { unmount } = render(
      <TalaDBProvider name="app.db">
        <Probe />
      </TalaDBProvider>,
    )
    await waitFor(() => expect(resolveOpen).toBeDefined())
    unmount() // cancelled before the open resolves

    resolveOpen!(makeDb())
    await waitFor(() => expect(closeSpy).toHaveBeenCalledTimes(1))
  })

  /**
   * The bug this guards: `openDB` is async, so a StrictMode mount-unmount-mount
   * used to leave two opens of the same database in flight at once. Only one can
   * hold the OPFS lock — the other concludes it is a second tab, falls back to an
   * IndexedDB snapshot, and forwards its writes to a primary that the teardown
   * then closes. The writes vanish, with no error and no trace beyond the data
   * simply not being there. Next.js enables StrictMode in development by default,
   * so this was the common case, not an edge one.
   */
  it('opens the database once under a StrictMode double-mount', async () => {
    render(
      <StrictMode>
        <TalaDBProvider name="app.db">
          <Probe />
        </TalaDBProvider>
      </StrictMode>,
    )

    await waitFor(() => expect(resolveOpen).toBeDefined())
    resolveOpen!(makeDb())
    await waitFor(() => expect(screen.getByTestId('ready')).toBeDefined())

    expect(openCalls).toHaveLength(1)
    // And the surviving mount still holds a live handle.
    expect(closeSpy).not.toHaveBeenCalled()
  })

  it('shares one handle between two providers naming the same database', async () => {
    render(
      <>
        <TalaDBProvider name="app.db">
          <Probe />
        </TalaDBProvider>
        <TalaDBProvider name="app.db">
          <Probe />
        </TalaDBProvider>
      </>,
    )

    await waitFor(() => expect(resolveOpen).toBeDefined())
    resolveOpen!(makeDb())
    await waitFor(() => expect(screen.getAllByTestId('ready')).toHaveLength(2))

    expect(openCalls).toHaveLength(1)
  })

  it('opens separate databases for different names', async () => {
    render(
      <TalaDBProvider name="one.db">
        <Probe />
      </TalaDBProvider>,
    )
    await waitFor(() => expect(openCalls).toHaveLength(1))

    render(
      <TalaDBProvider name="two.db">
        <Probe />
      </TalaDBProvider>,
    )
    await waitFor(() => expect(openCalls).toHaveLength(2))
    expect(openCalls.map((call) => call.name)).toEqual(['one.db', 'two.db'])
  })

  it('db-prop form still works unchanged', () => {
    render(
      <TalaDBProvider db={makeDb()}>
        <Probe />
      </TalaDBProvider>,
    )
    expect(screen.getByTestId('ready').textContent).toBe('db-ready')
  })
})

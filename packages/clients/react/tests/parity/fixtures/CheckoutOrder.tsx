/**
 * The one place `mutationFn` genuinely works: `mode: 'immediate'`.
 *
 * A checkout is the write you must not show as succeeded before the server
 * agrees, so it is sent first and stored only on success — a plain request with
 * no queue behind it, and therefore nothing to roll back. That also means the
 * closure is called from *here*, while this component is alive, which is
 * exactly why `mutationFn` is possible in this mode and impossible in the other
 * two.
 *
 * What this fixture pins:
 *
 * - `mutationFn` accepted under `immediate`, with TanStack's signature.
 * - `isPending` genuinely reflecting the request, unlike under `optimistic`.
 * - `mutateAsync` awaited inside an event handler, throwing on failure — the
 *   form of every "await the save, then navigate" handler.
 * - `data` being the stored document, taken from the server's response body.
 */
import { useMutation } from '@taladb/react/query'
import type { Order } from './todo'

export function CheckoutOrder({ cart, onPlaced }: { cart: Order; onPlaced: (id: string) => void }) {
  const { mutateAsync, isPending, data, isError, error } = useMutation<Order>({
    collection: 'orders',
    mode: 'immediate',
    mutationFn: async (order: Order) => {
      const response = await fetch('/api/orders', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(order),
      })
      if (!response.ok) throw new Error(`Checkout failed (${response.status})`)
      return (await response.json()) as Order
    },
  })

  return (
    <section>
      <button
        type="button"
        disabled={isPending}
        onClick={async () => {
          try {
            const placed = await mutateAsync(cart)
            onPlaced(placed._id)
          } catch {
            // Rendered from `error` below; nothing was written locally.
          }
        }}
      >
        {isPending ? 'Placing…' : 'Place order'}
      </button>
      {isError ? <p role="alert">{error.message}</p> : null}
      {data ? <p data-testid="placed">{data._id}</p> : null}
    </section>
  )
}

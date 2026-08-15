/**
 * The canonical TanStack Query list component, unmodified except for its import.
 *
 * Every line here is load-bearing for parity:
 *
 * - No `collection` option — it must be inferred from `queryKey[0]` (plan §2.2).
 * - No `staleTime` — it must default to an hour, and a warm mount must not
 *   refetch (plan §2.4).
 * - `if (isPending)` must *narrow* `data` to `Todo[]` below it, or `data.map`
 *   does not compile. That makes the result a discriminated union on `status`,
 *   not a flat interface with `data: T | undefined`.
 * - `error.message` requires `error` to be typed `Error`, not `unknown`.
 */
import { useQuery } from '@taladb/react/query'
import { fetchTodos, type Todo } from './todo'

export function TodoList() {
  const { data, error, isPending, isError, isFetching, refetch } = useQuery({
    queryKey: ['todos'],
    queryFn: fetchTodos,
  })

  if (isPending) return <p role="status">Loading…</p>
  if (isError) return <p role="alert">{error.message}</p>

  return (
    <section>
      {isFetching ? <span data-testid="refreshing">Refreshing…</span> : null}
      <ul>
        {data.map((todo: Todo) => (
          <li key={todo._id}>{todo.title}</li>
        ))}
      </ul>
      <button type="button" onClick={() => void refetch()}>
        Refresh
      </button>
    </section>
  )
}

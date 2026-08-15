/**
 * Pagination, written the v5 way.
 *
 * What this fixture pins:
 *
 * - `placeholderData: keepPreviousData` — v5 replaced the `keepPreviousData`
 *   *option* with this identity function, so the subpath has to export it.
 *   Placeholder data must never reach the collection: showing page 1's rows
 *   while page 2 loads is a render-time courtesy, not a fetch result.
 * - `isPlaceholderData`, which is the only way the component can tell.
 * - An object segment in the key (`{ page }`), which must hash stably across
 *   renders — `hashQueryKey` already sorts object properties for this reason.
 * - `retry` and `refetchOnWindowFocus` accepted and honoured on the read path.
 */
import { keepPreviousData, useQuery } from '@taladb/react/query'
import { fetchTodoPage, type Todo } from './todo'

export function TodoPage({ page }: { page: number }) {
  const { data, error, isPending, isError, isPlaceholderData, isFetching } = useQuery({
    queryKey: ['todos', { page }],
    queryFn: ({ signal }) => fetchTodoPage(page, signal),
    placeholderData: keepPreviousData,
    staleTime: 30_000,
    retry: 2,
    refetchOnWindowFocus: false,
  })

  if (isPending) return <p role="status">Loading…</p>
  // Both guards are required, here and in TanStack alike: `isPending` alone
  // leaves the loading-error case, where `data` is undefined.
  if (isError) return <p role="alert">{error.message}</p>

  return (
    <section data-stale={isPlaceholderData ? 'true' : 'false'}>
      {isFetching ? <span data-testid="refreshing">Refreshing…</span> : null}
      <ul>
        {data.map((todo: Todo) => (
          <li key={todo._id}>{todo.title}</li>
        ))}
      </ul>
    </section>
  )
}

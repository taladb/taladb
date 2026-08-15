/**
 * A detail view: one document, fetched by an id that may not exist yet.
 *
 * What this fixture pins:
 *
 * - A `queryFn` returning a single document rather than an array. `data` must
 *   be that document, not `[document]` (plan §3.3).
 * - The fetch context carries `queryKey` and `signal`, and `queryKey` survives
 *   destructuring with its tuple shape intact.
 * - `enabled` gates the fetch; while disabled the query stays `pending` and
 *   `data` stays `undefined` rather than resolving empty.
 * - `select` transforms what the component sees without changing what is
 *   stored — the collection still holds the whole document.
 * - `isLoading` (not `isPending`) is the guard, so the two must differ:
 *   `isLoading === isPending && isFetching`.
 */
import { useQuery } from '@taladb/react/query'
import { fetchTodo, type Todo } from './todo'

interface TodoSummary {
  title: string
  overdue: boolean
}

export function TodoDetail({ todoId }: { todoId: string | null }) {
  const { data, error, isLoading, isError } = useQuery({
    queryKey: ['todos', todoId] as const,
    queryFn: ({ queryKey, signal }) => {
      const [, id] = queryKey
      return fetchTodo(id as string, signal)
    },
    enabled: todoId !== null,
    staleTime: 5 * 60_000,
    select: (todo: Todo): TodoSummary => ({
      title: todo.title,
      overdue: todo.dueAt !== null && Date.parse(todo.dueAt) < Date.now(),
    }),
  })

  if (todoId === null) return <p>Pick a todo.</p>
  if (isLoading) return <p role="status">Loading…</p>
  if (isError) return <p role="alert">{error.message}</p>

  return (
    <article>
      <h1>{data?.title}</h1>
      {data?.overdue ? <em data-testid="overdue">Overdue</em> : null}
    </article>
  )
}

/**
 * An update, driven by per-call callbacks rather than hook-level ones.
 *
 * What this fixture pins:
 *
 * - `operation: 'update'`, declared once. It is deliberately *not* inferred
 *   from the presence of `_id` — a caller who passes an id on an insert would
 *   silently get an update instead.
 * - `mutate(variables, { onSuccess, onError })`. The per-call form is used
 *   heavily wherever the callback depends on which row was clicked, and it
 *   composes with the hook-level callbacks rather than replacing them.
 * - `variables`, `status` and `reset()` — the parts of the result object that
 *   exist only on mutations. `status` carries an `'idle'` that `useQuery` has
 *   no equivalent of.
 * - The optimistic claim itself: the list re-renders from the local commit with
 *   no `invalidateQueries` anywhere, because the collection *is* the state.
 */
import { useMutation } from '@taladb/react/query'
import type { Todo } from './todo'

export function ToggleTodo({ todo, onToggled }: { todo: Todo; onToggled?: (id: string) => void }) {
  const { mutate, status, variables, reset } = useMutation<Todo>({
    collection: 'todos',
    url: '/api/todos/:id',
    operation: 'update',
  })

  return (
    <li>
      <label>
        <input
          type="checkbox"
          checked={todo.done}
          onChange={() =>
            mutate(
              { _id: todo._id, done: !todo.done },
              { onSuccess: (updated) => onToggled?.(updated._id) },
            )
          }
        />
        {todo.title}
      </label>
      <span data-testid={`status-${todo._id}`}>{status}</span>
      {status === 'error' ? (
        <button type="button" onClick={() => reset()}>
          Dismiss ({String(variables?._id)})
        </button>
      ) : null}
    </li>
  )
}

/**
 * The canonical create-and-navigate mutation, unmodified except for its import
 * and the two options this layer needs.
 *
 * What this fixture pins:
 *
 * - `mutate(variables)` takes the *document*, as TanStack's does. The operation
 *   is declared once on the hook, not passed at every call site.
 * - `isPending` disables the button — so the field has to exist even though it
 *   stays `false` under the default optimistic mode, which is the point of that
 *   mode.
 * - `onSuccess(data, variables)` fires with the stored document. Navigating
 *   from `onSuccess` is the single most common thing mutation callbacks are
 *   used for, and it is why the callback cannot wait for the server: under
 *   `optimistic` the request has not been sent yet.
 * - `error.message` requires `error` to be typed `Error`, not `unknown`.
 *
 * `collection` and `url` are the two additions a migrating codebase must make.
 * Everything else on this page is TanStack v5 as written.
 */
import { useMutation } from '@taladb/react/query'
import { useState } from 'react'
import type { Todo } from './todo'

export function CreateTodo({ onCreated }: { onCreated: (todo: Todo) => void }) {
  const [title, setTitle] = useState('')

  const { mutate, isPending, isError, error } = useMutation<Todo>({
    collection: 'todos',
    url: '/api/todos/:id',
    onSuccess: (todo) => {
      setTitle('')
      onCreated(todo)
    },
  })

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault()
        mutate({ title, done: false, dueAt: null })
      }}
    >
      <input aria-label="Title" value={title} onChange={(e) => setTitle(e.target.value)} />
      <button type="submit" disabled={isPending}>
        {isPending ? 'Saving…' : 'Add'}
      </button>
      {isError ? <p role="alert">{error.message}</p> : null}
    </form>
  )
}

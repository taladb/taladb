/**
 * Shared types and fetchers for the parity fixtures.
 *
 * Written as an application would write them: plain `fetch` calls that throw on
 * a non-2xx and return parsed JSON. Nothing here knows the query layer exists —
 * that is the point, since a migrating codebase's data-access module is exactly
 * the file that should not have to change.
 */

export interface Todo {
  _id: string
  title: string
  done: boolean
  dueAt: string | null
}

export async function fetchTodos({ signal }: { signal?: AbortSignal }): Promise<Todo[]> {
  const response = await fetch('/api/todos', { signal })
  if (!response.ok) throw new Error(`Failed to load todos (${response.status})`)
  return (await response.json()) as Todo[]
}

export async function fetchTodo(id: string, signal?: AbortSignal): Promise<Todo> {
  const response = await fetch(`/api/todos/${id}`, { signal })
  if (!response.ok) throw new Error(`Failed to load todo ${id} (${response.status})`)
  return (await response.json()) as Todo
}

export async function fetchTodoPage(page: number, signal?: AbortSignal): Promise<Todo[]> {
  const response = await fetch(`/api/todos?page=${page}`, { signal })
  if (!response.ok) throw new Error(`Failed to load page ${page} (${response.status})`)
  return (await response.json()) as Todo[]
}

/** For the `immediate` fixture — the write you must not show as done early. */
export interface Order {
  _id: string
  total: number
  items: string[]
}

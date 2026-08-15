import { useCallback, useEffect, useRef, useState } from 'react'
import type { Document } from 'taladb'
import { useCollection } from '../useCollection'
import { withoutEnvelope } from './canonical'
import { useQueryContext } from './context'
import { DEFAULT_METHODS, defaultClassify, resolveUrl } from './endpoint'
import { newDocId } from './keys'
import { writeConfirmed, writeLocal } from './mutate'
import { isOffline, retryDelayMs, shouldRetry, sleep } from './retry'
import type {
  DeleteVariables,
  NetworkMode,
  RetryDelayOption,
  RetryOption,
  Doc,
  InsertVariables,
  MutationCallbacks,
  MutationOp,
  MutationResult,
  MutationResultCommon,
  UpdateVariables,
  UseMutationOptions,
} from './types'
import { checkMutationFilter, isDev } from './validate'

/** Everything the hook tracks about the last write. */
interface MutationState<T> {
  status: 'idle' | 'pending' | 'success' | 'error'
  data: T | undefined
  error: Error | null
  variables: unknown
  submittedAt: number
  failureCount: number
  failureReason: Error | null
  /** Offline, so an `immediate` write is waiting for the connection. */
  paused: boolean
}

const INITIAL: MutationState<never> = {
  status: 'idle',
  data: undefined,
  error: null,
  variables: undefined,
  submittedAt: 0,
  failureCount: 0,
  failureReason: null,
  paused: false,
}

/** Non-`Error` throws are wrapped, so `error.message` always works. */
function toError(cause: unknown): Error {
  return cause instanceof Error ? cause : new Error(String(cause))
}

/**
 * Write to a collection.
 *
 * `mutate(variables)` takes the document — TanStack's signature — and the
 * operation is declared once on the hook:
 *
 * ```ts
 * const createTodo = useMutation<Todo>({ collection: 'todos', url: '/api/todos/:id' })
 * createTodo.mutate({ title: 'Buy milk' })
 *
 * const updateTodo = useMutation<Todo>({ collection: 'todos', url: '/api/todos/:id', operation: 'update' })
 * updateTodo.mutate({ _id, done: true })
 * ```
 *
 * `url` is a template rather than a function for one reason: under `optimistic`
 * and `queued` the request is sent by the drain loop, long after this component
 * is gone — after a route change, a reload, a week. A template is data and can
 * be stored on the queued document; a closure cannot. That is also why
 * `mutationFn` is only possible under `immediate`.
 *
 * Auth and error policy stay on `<QueryProvider>`. `headers` in particular has
 * to be resolved at send time, so a write queued offline carries a current
 * token instead of the expired one it was made with.
 */
// `mutationFn` given: the variables are whatever it takes, decoupled from the
// document type as they are in TanStack. A submit form's input is very often
// not the row that comes back.
// `TVariables` defaults to `T` rather than being inferred only: a call site
// that writes `useMutation<Order>(…)` supplies one type argument, and an
// overload requiring two would be skipped entirely.
export function useMutation<T extends Doc, TVariables = T>(
  options: UseMutationOptions<T, TVariables> & {
    mode: 'immediate'
    mutationFn: (variables: TVariables) => Promise<T>
  },
): MutationResult<T, TVariables>
// An insert: no `_id` required — it is minted client-side.
export function useMutation<T extends Doc = Doc>(
  options: UseMutationOptions<T, InsertVariables<T>> & { operation?: 'insert' },
): MutationResult<T, InsertVariables<T>>
// An update: `_id` required, every other field optional.
export function useMutation<T extends Doc = Doc>(
  options: UseMutationOptions<T, UpdateVariables<T>> & { operation: 'update' },
): MutationResult<T, UpdateVariables<T>>
// A delete: `_id` and nothing else.
export function useMutation<T extends Doc = Doc>(
  options: UseMutationOptions<T, DeleteVariables> & { operation: 'delete' },
): MutationResult<T, DeleteVariables>
export function useMutation<T extends Doc = Doc, TVariables = InsertVariables<T>>(
  options: UseMutationOptions<T, TVariables>,
): MutationResult<T, TVariables> {
  const {
    collection: name,
    url,
    method,
    mode = 'optimistic',
    operation = 'insert',
  } = options
  // Checked during render, not at call time: a component wired this way is
  // broken however it is used, and finding out on the first click — possibly in
  // production — is worse than finding out on the first paint.
  if (options.mutationFn !== undefined && mode !== 'immediate') {
    throw new Error(
      `useMutation({ collection: '${name}' }) was given a \`mutationFn\` in ` +
        `'${mode}' mode. Under '${mode}' the request is sent by the drain loop, long after ` +
        'this component is gone — after a route change, a reload, a week — so there is no ' +
        'closure left to call and the write would be silently dropped.\n\n' +
        'Either route it by template, which is data and survives:\n' +
        `  useMutation({ collection: '${name}', url: '/api/${name}/:id' })\n\n` +
        'or send it from here and store only what the server agreed to:\n' +
        `  useMutation({ collection: '${name}', mode: 'immediate', mutationFn })`,
    )
  }

  const { backend, register, requestDrain, noteWrite } = useQueryContext()
  // The engine's constraint, applied where documents are actually stored. The
  // public generic is looser on purpose — see `Doc`.
  const collection = useCollection<Document>(name)

  const [state, setState] = useState<MutationState<T>>(INITIAL as MutationState<T>)

  // Read through a ref so an inline `onSuccess={() => …}` does not change the
  // identity of `mutate` on every render.
  const callbacksRef = useRef(options)
  callbacksRef.current = options

  // Declared on mount, not on first write, so a queue left from a previous
  // session drains as soon as this component appears.
  useEffect(() => {
    register(name)
  }, [register, name])

  // Live subscriptions waiting for the drain to confirm a write. Torn down on
  // unmount, which is what makes `onSynced` best-effort rather than a leak.
  const watchersRef = useRef<Set<() => void>>(new Set())

  const mountedRef = useRef(true)
  useEffect(() => {
    mountedRef.current = true
    return () => {
      mountedRef.current = false
      for (const stop of watchersRef.current) stop()
      watchersRef.current.clear()
    }
  }, [])

  const patch = useCallback((next: Partial<MutationState<T>>) => {
    if (mountedRef.current) setState((current) => ({ ...current, ...next }))
  }, [])

  const reset = useCallback(() => {
    if (mountedRef.current) setState(INITIAL as MutationState<T>)
  }, [])

  const mutateAsync = useCallback(
    async (variables: TVariables, perCall?: MutationCallbacks<T, TVariables>): Promise<T> => {
      const hooks = callbacksRef.current
      const op = toOperation<T, TVariables>(operation, variables)
      if (isDev() && op.type !== 'insert') checkMutationFilter(op.where)

      patch({
        status: 'pending',
        variables,
        error: null,
        data: undefined,
        submittedAt: Date.now(),
      })

      let context: unknown
      try {
        context = await hooks.onMutate?.(variables)

        const data =
          mode === 'immediate'
            ? await runScoped(hooks.scope?.id, () =>
                attemptWrite({
                  retry: hooks.retry,
                  retryDelay: hooks.retryDelay,
                  networkMode: hooks.networkMode,
                  onPaused: (paused) => patch({ paused }),
                  onFailure: (count, reason) => patch({ failureCount: count, failureReason: reason }),
                  send: () =>
                    sendThenStore<T>({
                      collection,
                      name,
                      op,
                      url,
                      method,
                      backend,
                      mutationFn: hooks.mutationFn as
                        | ((variables: unknown) => Promise<T>)
                        | undefined,
                      variables,
                    }),
                }),
              )
            : await storeThenSend<T>({ collection, op, url, method, mode, requestDrain, noteWrite })

        patch({
          status: 'success',
          data,
          error: null,
          failureCount: 0,
          failureReason: null,
          paused: false,
        })
        // Hook-level first, then per-call: the per-call form adds to the
        // hook's behaviour rather than replacing it.
        await hooks.onSuccess?.(data, variables, context)
        await perCall?.onSuccess?.(data, variables, context)
        await hooks.onSettled?.(data, null, variables, context)
        await perCall?.onSettled?.(data, null, variables, context)

        if (mode === 'immediate') {
          // The server has already agreed, so the two callbacks mean the same
          // thing here.
          await hooks.onSynced?.(data, variables)
          await perCall?.onSynced?.(data, variables)
        } else if (hooks.onSynced !== undefined || perCall?.onSynced !== undefined) {
          watchUntilSynced(collection, (data as { _id?: string })._id, watchersRef, () => {
            void hooks.onSynced?.(data, variables)
            void perCall?.onSynced?.(data, variables)
          })
        }
        return data
      } catch (cause: unknown) {
        const error = toError(cause)
        setState((current) => ({
          ...current,
          status: 'error',
          data: undefined,
          error,
          failureCount: Math.max(current.failureCount, 1),
          failureReason: error,
          paused: false,
        }))
        await hooks.onError?.(error, variables, context)
        await perCall?.onError?.(error, variables, context)
        await hooks.onSettled?.(undefined, error, variables, context)
        await perCall?.onSettled?.(undefined, error, variables, context)
        throw error
      }
    },
    [collection, name, mode, operation, url, method, backend, requestDrain, noteWrite, patch],
  )

  const mutate = useCallback(
    (variables: TVariables, perCall?: MutationCallbacks<T, TVariables>): void => {
      void mutateAsync(variables, perCall).catch(() => {
        /* surfaced on `error`; swallowed so it is not an unhandled rejection */
      })
    },
    [mutateAsync],
  )

  const common: MutationResultCommon<T, TVariables> = {
    mutate,
    mutateAsync,
    reset,
    // `immediate` is the only mode that can be blocked by connectivity; the
    // others commit locally and let the drain worry about the network.
    isPaused: state.paused,
    failureCount: state.failureCount,
    failureReason: state.failureReason,
    submittedAt: state.submittedAt,
  }

  const result = buildResult<T, TVariables>(common, state)

  const throwOnError = options.throwOnError ?? false
  if (
    state.error !== null &&
    (typeof throwOnError === 'function' ? throwOnError(state.error) : throwOnError)
  ) {
    throw state.error
  }

  return result
}

/**
 * Send, retrying and pausing per policy.
 *
 * Only ever wraps `immediate`. A queued write's backoff lives on the document
 * in `drain.ts`, where it survives a reload — this one is in-memory and dies
 * with the component, which is the right shape for a request the user is
 * watching and the wrong shape for one they are not.
 */
async function attemptWrite<T>(args: {
  retry: RetryOption | undefined
  retryDelay: RetryDelayOption | undefined
  networkMode: NetworkMode | undefined
  onPaused: (paused: boolean) => void
  onFailure: (count: number, reason: Error) => void
  send: () => Promise<T>
}): Promise<T> {
  const { retry, retryDelay, networkMode, onPaused, onFailure, send } = args
  const controller = new AbortController()
  let failures = 0

  for (;;) {
    // Pause rather than fail: a write the user just made should wait for the
    // connection, not be thrown away because it happened during a tunnel.
    if ((networkMode ?? 'online') === 'online' && isOffline()) {
      onPaused(true)
      await waitForOnline()
      onPaused(false)
    }

    try {
      return await send()
    } catch (cause: unknown) {
      const error = cause instanceof Error ? cause : new Error(String(cause))
      failures += 1
      onFailure(failures, error)
      // Default `0` — a write may not be idempotent, so retrying is opt-in.
      if (!shouldRetry(retry ?? 0, failures, error)) throw error
      await sleep(retryDelayMs(retryDelay, failures - 1, error), controller.signal)
    }
  }
}

/** Resolves when the browser says the connection is back. */
function waitForOnline(): Promise<void> {
  if (typeof window === 'undefined') return Promise.resolve()
  return new Promise((resolve) => {
    const onOnline = () => {
      window.removeEventListener('online', onOnline)
      resolve()
    }
    window.addEventListener('online', onOnline)
  })
}

/**
 * Run mutations sharing a scope id one at a time, in call order.
 *
 * Module-level because the guarantee is about the *scope*, not about one
 * component: two rows of a list, each with its own hook, must still serialise
 * if they name the same scope.
 */
const scopeChains = new Map<string, Promise<unknown>>()

function runScoped<T>(id: string | undefined, work: () => Promise<T>): Promise<T> {
  if (id === undefined) return work()

  const previous = scopeChains.get(id) ?? Promise.resolve()
  // A failed mutation must not stall everything queued behind it, so the chain
  // is built on the settled form.
  const next = previous.then(work, work)
  const settled = next.then(
    () => undefined,
    () => undefined,
  )
  scopeChains.set(id, settled)
  void settled.then(() => {
    // Only clear the slot if nothing joined behind us.
    if (scopeChains.get(id) === settled) scopeChains.delete(id)
  })
  return next
}

/** Test seam — scope chains are module state that would leak between cases. */
export function resetMutationScopes(): void {
  scopeChains.clear()
}

/**
 * Watch one document until the drain marks it synced, then fire once.
 *
 * A live query rather than a hook into the drain, because the drain is
 * provider-wide and per-collection while this cares about a single document —
 * and because the confirmation may arrive from another tab's drain, which only
 * the database sees.
 */
function watchUntilSynced(
  collection: ReturnType<typeof useCollection<Document>>,
  id: string | undefined,
  watchers: { current: Set<() => void> },
  fire: () => void,
): void {
  if (id === undefined) return
  let stop: (() => void) | undefined
  let done = false

  const finish = () => {
    if (done) return
    done = true
    if (stop !== undefined) {
      watchers.current.delete(stop)
      stop()
    }
  }

  stop = collection.subscribe({ _id: id } as never, (docs) => {
    const doc = docs[0] as { _sync?: string } | undefined
    // Absent means the row is gone — a confirmed delete, or an insert that was
    // cancelled before it ever went out. Neither is a sync to report.
    if (doc === undefined) {
      finish()
      return
    }
    if (doc._sync === 'synced') {
      finish()
      fire()
    }
  })
  watchers.current.add(stop)
}

/**
 * Turn plain variables into the storage layer's write intent.
 *
 * The operation comes from the hook rather than from the shape of the
 * variables. Inferring "has `_id` ⇒ update" would turn an insert that happens
 * to carry a client-generated id into a silent update of whatever it collided
 * with.
 */
function toOperation<T extends Doc, TVariables>(
  operation: 'insert' | 'update' | 'delete',
  variables: TVariables,
): MutationOp<T> {
  const values = (variables ?? {}) as Record<string, unknown>

  if (operation === 'insert') return { type: 'insert', doc: values as never }

  const id = values._id
  if (typeof id !== 'string' || id === '') {
    throw new Error(
      `useMutation({ operation: '${operation}' }) needs an \`_id\` on the variables — ` +
        `mutate({ _id, … }). Writes are addressed by id, never by filter: a filter is ` +
        're-evaluated at the primary tab against data the calling tab has not seen.',
    )
  }

  if (operation === 'delete') return { type: 'delete', where: { _id: id } }

  const { _id: _ignored, ...set } = values
  return { type: 'update', where: { _id: id }, set: set as never }
}

/** `optimistic` and `queued`: commit locally, then let the drain send it. */
async function storeThenSend<T extends Doc>(args: {
  collection: ReturnType<typeof useCollection<Document>>
  op: MutationOp<T>
  url?: string
  method?: Partial<Record<'insert' | 'update' | 'delete', string>>
  mode: 'optimistic' | 'queued' | 'immediate'
  requestDrain: (force?: boolean) => void
  noteWrite: () => void
}): Promise<T> {
  const { collection, op, url, method, mode, requestDrain, noteWrite } = args
  const written = await writeLocal(collection, op as never, Date.now(), { url, method })

  // `queued` waits for the provider's schedule; `optimistic` asks for a cycle
  // now. Either way the write is already committed and durable, and every
  // query over these documents has already re-rendered.
  if (mode === 'optimistic') requestDrain()
  // `queued` deliberately does not ask for a cycle — but it must still be
  // counted, or a backlog under a long interval would never trip the threshold.
  else noteWrite()

  // The stored document is what `data` reports — for a delete there is a
  // tombstone rather than a row the caller would want back, so the id stands
  // in for it.
  const stored = await collection.findOne({ _id: written.id } as never)
  return (stored === null ? ({ _id: written.id } as T) : (withoutEnvelope(stored) as T))
}

/** `immediate`: send first, store only what the server agreed to. */
async function sendThenStore<T extends Doc>(args: {
  collection: ReturnType<typeof useCollection<Document>>
  name: string
  op: MutationOp<T>
  url?: string
  method?: Partial<Record<'insert' | 'update' | 'delete', string>>
  backend: ReturnType<typeof useQueryContext>['backend']
  mutationFn?: (variables: unknown) => Promise<T>
  variables: unknown
}): Promise<T> {
  const { collection, name, op, url, method, backend, mutationFn, variables } = args

  // `mutationFn` owns the request outright — `url`, `method` and `classify` are
  // this layer's way of describing a request it makes itself, and it is not
  // making this one.
  if (mutationFn !== undefined) {
    const returned = await mutationFn(variables)
    const id =
      (returned as { _id?: unknown } | null)?._id !== undefined
        ? String((returned as { _id: unknown })._id)
        : op.type === 'insert'
          ? ((op.doc._id as string) ?? newDocId(name))
          : op.where._id

    if (op.type === 'delete') {
      await collection.deleteOne({ _id: id } as never)
      return { _id: id } as T
    }
    const stored = { ...(returned as object), _id: id } as T
    await writeConfirmed(collection, stored as never, Date.now())
    return stored
  }

  if (url === undefined) {
    throw new Error(
      "useMutation({ mode: 'immediate' }) needs a `url` — the request is sent " +
        'from here, so there is nothing to route it by otherwise.',
    )
  }

  // The id exists before the request, not after: client ids are authoritative,
  // and a retried insert only avoids duplicating if it carries the same id
  // both times.
  const id = op.type === 'insert' ? ((op.doc._id as string) ?? newDocId(name)) : op.where._id
  const body =
    op.type === 'delete'
      ? null
      : op.type === 'insert'
        ? { ...op.doc, _id: id }
        : { ...op.set, _id: id }

  const verb = method?.[op.type] ?? backend?.method[op.type] ?? DEFAULT_METHODS[op.type]
  const headers = new Headers(backend?.headers ? await backend.headers() : undefined)
  if (body !== null && !headers.has('content-type')) {
    headers.set('content-type', 'application/json')
  }

  const doFetch = backend?.fetch ?? globalThis.fetch
  const response = await doFetch(resolveUrl(url, name, id, op.type), {
    method: verb,
    headers,
    body: body === null ? undefined : JSON.stringify(body),
  })

  const outcome = backend?.classify
    ? backend.classify(response, { collection: name, id, type: op.type, document: body })
    : defaultClassify(response)

  if (outcome === 'retry' || outcome === 'terminal') {
    throw new Error(
      `The server rejected this ${op.type} with status ${response.status}. ` +
        'Nothing was written locally.',
    )
  }

  if (op.type === 'delete') {
    await collection.deleteOne({ _id: id } as never)
    return { _id: id } as T
  }

  // The response body is canonical, so server-set fields land without a second
  // fetch. A 409/empty response carries none; for an update, merge the
  // submitted fields into the last local shape so a contract violation cannot
  // erase unrelated fields.
  const canonical = await readJson(response)
  const fallback =
    op.type === 'update'
      ? { ...withoutEnvelope((await collection.findOne({ _id: id } as never)) ?? {}), ...body }
      : body
  const stored = {
    ...(canonical && Object.keys(canonical).length > 0 ? canonical : fallback),
    _id: id,
  } as T
  await writeConfirmed(collection, stored as never, Date.now())
  return stored
}

/**
 * Assemble the status-specific half of the result.
 *
 * One function returning the union, so the compiler checks each member against
 * its declared literal flags — spread inline, an `isSuccess: true` beside
 * `status: 'error'` would type-check happily.
 */
function buildResult<T extends Doc, TVariables>(
  common: MutationResultCommon<T, TVariables>,
  state: MutationState<T>,
): MutationResult<T, TVariables> {
  if (state.status === 'error') {
    return {
      ...common,
      status: 'error',
      data: undefined,
      error: state.error as Error,
      variables: state.variables as TVariables,
      isIdle: false,
      isPending: false,
      isSuccess: false,
      isError: true,
    }
  }
  if (state.status === 'success') {
    return {
      ...common,
      status: 'success',
      data: state.data as T,
      error: null,
      variables: state.variables as TVariables,
      isIdle: false,
      isPending: false,
      isSuccess: true,
      isError: false,
    }
  }
  if (state.status === 'pending') {
    return {
      ...common,
      status: 'pending',
      data: undefined,
      error: null,
      variables: state.variables as TVariables,
      isIdle: false,
      isPending: true,
      isSuccess: false,
      isError: false,
    }
  }
  return {
    ...common,
    status: 'idle',
    data: undefined,
    error: null,
    variables: undefined,
    isIdle: true,
    isPending: false,
    isSuccess: false,
    isError: false,
  }
}

async function readJson(response: Response): Promise<Record<string, unknown> | null> {
  try {
    const parsed: unknown = await response.json()
    return parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null
  } catch {
    return null
  }
}

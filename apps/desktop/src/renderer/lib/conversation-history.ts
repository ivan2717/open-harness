import { useEffect, useMemo, useRef, useState } from 'react'
import type { ChatMessage, RunDetail, TimelineEvent } from '../../shared/contracts'
import { errorMessage } from './format'

/** Earlier pages precede the live tail; the live version wins on overlap. */
export function mergeHistory<T>(earlier: T[], recent: T[], key: (item: T) => string | number): T[] {
  const items = new Map(earlier.map((item) => [key(item), item]))
  for (const item of recent) items.set(key(item), item)
  return [...items.values()]
}

export function hasHistoryGap<T>(
  earlier: T[],
  recent: T[],
  key: (item: T) => string | number,
  before: string | number | null | undefined,
): boolean {
  if (before == null || !earlier.length || !recent.length) return false
  const previous = new Set(earlier.map(key))
  return !recent.some((item) => previous.has(key(item)))
}

type LoadedHistory = {
  id: string
  before: string | null
  messages: ChatMessage[]
  events: TimelineEvent[]
}
const messageKey = (message: ChatMessage) => message.id
const eventKey = (event: TimelineEvent) => event.sequence

export function mergeMessageHistory(earlier: ChatMessage[], recent: ChatMessage[]): ChatMessage[] {
  // Provisional stream IDs disappear when the canonical response is committed
  // or interrupted; a union by ID alone would preserve their spinner forever.
  return mergeHistory(
    earlier.filter((message) => message.status !== 'streaming'),
    recent,
    messageKey,
  )
}

type PendingRefresh = { id: string; before: string }

export function hiddenPendingMessages(
  messages: ChatMessage[],
  live: ChatMessage[],
): PendingRefresh[] {
  const liveIds = new Set(live.map(messageKey))
  const pending: PendingRefresh[] = []
  let next: string | undefined
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index]
    if (message.status === 'streaming') continue
    if (message.status === 'pending' && !liveIds.has(message.id) && next) {
      pending.push({ id: message.id, before: next })
    }
    next = message.id
  }
  return pending
}

export function refreshHistoryMessages(
  earlier: ChatMessage[],
  page: ChatMessage[],
  live: ChatMessage[],
): ChatMessage[] {
  const updates = new Map(
    page
      .filter((message) => message.status !== 'streaming')
      .map((message) => [message.id, message]),
  )
  // A recheck updates existing rows only. The fetched page can contain other
  // messages outside the loaded range; appending them would reorder history.
  return mergeMessageHistory(
    earlier.map((message) => {
      const update = updates.get(message.id)
      if (message.status !== 'pending' && update?.status === 'pending') return message
      return update ?? message
    }),
    live,
  )
}

export async function refreshPendingMessages(
  runId: string,
  pending: PendingRefresh[],
  readPage: (id: string, before: string) => Promise<RunDetail>,
  apply: (messages: ChatMessage[]) => void,
  active: () => boolean,
): Promise<void> {
  const seen = new Set<string>()
  for (const target of pending) {
    if (!active()) return
    if (seen.has(target.id)) continue
    let before: string | null = target.before
    const visited = new Set<string>()
    while (before && !visited.has(before)) {
      visited.add(before)
      const page = await readPage(runId, before)
      if (!active()) return
      apply(page.messages)
      for (const message of page.messages) seen.add(message.id)
      if (seen.has(target.id)) break
      // A very fast burst can move the live tail by more than one page. Walk
      // back across that gap instead of leaving the old pending row stranded.
      before = page.history?.before ?? null
    }
  }
}

export function reconcileConversationHistory(
  history: LoadedHistory,
  run: RunDetail,
): LoadedHistory {
  if (
    hasHistoryGap(
      history.messages.filter((message) => message.status !== 'streaming'),
      run.messages.filter((message) => message.status !== 'streaming'),
      messageKey,
      run.history?.before,
    )
  ) {
    // A disconnected subscriber can skip more than one live window. Keep one
    // contiguous tail and restore its paging cursor, never a silent union gap.
    return {
      id: run.id,
      before: run.history?.before ?? null,
      messages: run.messages,
      events: run.events,
    }
  }
  return {
    ...history,
    messages: mergeMessageHistory(history.messages, run.messages),
    events: mergeHistory(history.events, run.events, eventKey),
  }
}

export function prependConversationPage(
  history: LoadedHistory,
  page: RunDetail,
  run: RunDetail,
  cursor: string,
): LoadedHistory {
  const latest = reconcileConversationHistory(history, run)
  // A live-window reset can happen while the earlier page is in flight. Its
  // original boundary must still exist before that page may join this window.
  if (!latest.messages.some((message) => message.id === cursor)) return latest
  return {
    ...latest,
    before: page.history?.before ?? null,
    messages: mergeMessageHistory(page.messages, latest.messages),
    events: mergeHistory(page.events, latest.events, eventKey),
  }
}

export function refreshConversationHistory(
  history: LoadedHistory,
  messages: ChatMessage[],
  run: RunDetail,
): LoadedHistory {
  const latest = reconcileConversationHistory(history, run)
  return {
    ...latest,
    messages: refreshHistoryMessages(latest.messages, messages, run.messages),
  }
}

export function useConversationHistory(run: RunDetail) {
  const [loaded, setLoaded] = useState<LoadedHistory>()
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string>()
  const [refreshError, setRefreshError] = useState<string>()
  const current = useRef(run)
  current.current = run
  const generation = useRef(0)
  useEffect(() => {
    generation.current++
    setLoaded(undefined)
    setLoading(false)
    setError(undefined)
    setRefreshError(undefined)
  }, [run.id])
  useEffect(() => {
    // Only retain extra pages after the user asks for history. Ordinary live
    // viewing keeps the bounded tail supplied by the backend.
    setLoaded((old) => (old?.id === run.id ? reconcileConversationHistory(old, run) : old))
  }, [run.id, run.revision])
  const history = useMemo(
    () => (loaded?.id === run.id ? reconcileConversationHistory(loaded, run) : undefined),
    [loaded, run],
  )
  const pendingKey = JSON.stringify(
    history ? hiddenPendingMessages(history.messages, run.messages) : [],
  )
  useEffect(() => {
    const pending = JSON.parse(pendingKey) as PendingRefresh[]
    if (!pending.length) {
      setRefreshError(undefined)
      return
    }
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined
    const refresh = async () => {
      try {
        await refreshPendingMessages(
          run.id,
          pending,
          (id, before) => window.harness.getRunPage(id, before),
          (messages) => {
            setLoaded((old) => {
              if (cancelled || old?.id !== run.id || current.current.id !== run.id) return old
              return refreshConversationHistory(old, messages, current.current)
            })
          },
          () => !cancelled,
        )
        if (!cancelled) setRefreshError(undefined)
      } catch {
        // Never invent completion after a failed refresh. Running tasks retry;
        // a final state transition triggers one more authoritative recheck.
        if (!cancelled) setRefreshError('较早操作的状态暂未刷新，请稍后重新打开会话。')
      }
      if (!cancelled && run.status === 'running') timer = setTimeout(() => void refresh(), 1_000)
    }
    void refresh()
    return () => {
      cancelled = true
      clearTimeout(timer)
    }
  }, [run.id, run.status, pendingKey])
  const before = history ? history.before : run.history?.before
  return {
    run: history ? { ...run, messages: history.messages, events: history.events } : run,
    before,
    loading,
    error: error ?? refreshError,
    load: async () => {
      if (!before || loading) return
      const epoch = generation.current
      const captured = run
      setLoading(true)
      setError(undefined)
      try {
        const page = await window.harness.getRunPage(run.id, before)
        if (epoch !== generation.current || current.current.id !== captured.id) return
        setLoaded((old) => {
          const previous =
            old?.id === captured.id
              ? old
              : {
                  id: captured.id,
                  before: captured.history?.before ?? null,
                  messages: captured.messages,
                  events: captured.events,
                }
          return prependConversationPage(previous, page, current.current, before)
        })
      } catch (error) {
        if (epoch === generation.current) setError(errorMessage(error))
      } finally {
        if (epoch === generation.current) setLoading(false)
      }
    },
  }
}

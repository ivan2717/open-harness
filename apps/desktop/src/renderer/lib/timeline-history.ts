import { useEffect, useRef, useState } from 'react'
import type { RunDetail, RunEventPage } from '../../shared/contracts'
import { hasHistoryGap, mergeHistory } from './conversation-history'
import { errorMessage } from './format'

type Loaded = RunEventPage & { id: string; source: boolean }
const eventKey = (event: RunEventPage['events'][number]) => event.sequence

export function reconcileTimelineHistory(history: Loaded, run: RunDetail): Loaded {
  if (history.source) return history
  if (hasHistoryGap(history.events, run.events, eventKey, run.history?.events_before)) {
    return {
      id: run.id,
      source: false,
      before: run.history?.events_before ?? null,
      events: run.events,
    }
  }
  return { ...history, events: mergeHistory(history.events, run.events, eventKey) }
}

export function prependTimelinePage(
  history: Loaded,
  page: RunEventPage,
  run: RunDetail,
  cursor: number,
): Loaded {
  const latest = reconcileTimelineHistory(history, run)
  if (!latest.events.some((event) => event.sequence === cursor)) return latest
  return {
    ...latest,
    before: page.before,
    events: mergeHistory(page.events, latest.events, eventKey),
  }
}

export function useTimelineHistory(run?: RunDetail) {
  const [loaded, setLoaded] = useState<Loaded>()
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string>()
  const request = useRef(0)
  const current = useRef(run)
  current.current = run
  useEffect(() => {
    request.current++
    setLoaded(undefined)
    setLoading(false)
    setError(undefined)
  }, [run?.id])
  useEffect(() => {
    if (run) setLoaded((old) => (old?.id === run.id ? reconcileTimelineHistory(old, run) : old))
  }, [run?.id, run?.revision])
  const history = run && loaded?.id === run.id ? reconcileTimelineHistory(loaded, run) : undefined
  const events = history?.events ?? run?.events ?? []
  const before = history ? history.before : run?.history?.events_before
  const load = async (source?: number) => {
    const cursor = source === undefined ? before : source + 1
    if (!run || cursor == null || (loading && source === undefined)) return
    const token = ++request.current
    setLoading(true)
    setError(undefined)
    try {
      const page = await window.harness.getRunEventsPage(run.id, cursor)
      if (token !== request.current || current.current?.id !== run.id) return
      setLoaded(
        source !== undefined
          ? { id: run.id, before: page.before, source: true, events: page.events }
          : prependTimelinePage(
              history ?? {
                id: run.id,
                before: run.history?.events_before ?? null,
                source: false,
                events,
              },
              page,
              current.current,
              cursor,
            ),
      )
    } catch (error) {
      if (token === request.current && current.current?.id === run.id) setError(errorMessage(error))
    } finally {
      if (token === request.current && current.current?.id === run.id) setLoading(false)
    }
  }
  return {
    events,
    before,
    loading,
    error,
    source: history?.source,
    load,
    latest: () => {
      request.current++
      setLoaded(undefined)
      setError(undefined)
      setLoading(false)
    },
  }
}

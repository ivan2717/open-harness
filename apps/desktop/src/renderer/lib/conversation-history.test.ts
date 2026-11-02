import { describe, expect, it, vi } from 'vitest'
import type { ChatMessage, RunDetail, TimelineEvent } from '../../shared/contracts'
import {
  hiddenPendingMessages,
  mergeMessageHistory,
  prependConversationPage,
  reconcileConversationHistory,
  refreshConversationHistory,
  refreshHistoryMessages,
  refreshPendingMessages,
} from './conversation-history'
import { prependTimelinePage, reconcileTimelineHistory } from './timeline-history'

function message(id: string, status: ChatMessage['status'] = 'complete'): ChatMessage {
  return { id, status, role: status === 'streaming' ? 'assistant' : 'tool', text: id }
}

function page(messages: ChatMessage[], before: string | null = null): RunDetail {
  return {
    id: 'run',
    revision: 10,
    title: 'fixture',
    prompt: 'task',
    status: 'completed',
    model_id: 'fixture',
    created_at: '2026-11-02T00:00:00Z',
    updated_at: '2026-11-02T00:00:01Z',
    messages,
    events: [],
    context: null,
    history: { before, total_messages: 1000, total_events: 1000, events_before: null },
  }
}

const range = (start: number, end: number) =>
  Array.from({ length: end - start + 1 }, (_, offset) => start + offset)
const messages = (start: number, end: number) => range(start, end).map((id) => message(String(id)))
const events = (start: number, end: number): TimelineEvent[] =>
  range(start, end).map((sequence) => ({
    sequence,
    step: sequence,
    type: 'tool_completed',
    label: String(sequence),
    detail: '',
    timestamp: '2026-11-02T00:00:00Z',
  }))

describe('disconnected live history windows', () => {
  it('restores a conversation paging cursor after reconnect and can read every skipped message', () => {
    const previous = { id: 'run', before: null, messages: messages(0, 199), events: [] }
    const live = page(messages(401, 500), '401')
    let loaded = reconcileConversationHistory(previous, live)
    expect(loaded.messages).toEqual(live.messages)
    expect(loaded.before).toBe('401')
    for (let count = 0; loaded.before !== null && count < 10; count++) {
      const cursor = Number(loaded.before)
      const start = Math.max(0, cursor - 100)
      loaded = prependConversationPage(
        loaded,
        page(messages(start, cursor - 1), start > 0 ? String(start) : null),
        live,
        String(cursor),
      )
    }
    expect(loaded.before).toBeNull()
    expect(loaded.messages.map((item) => item.id)).toEqual(range(0, 500).map(String))
  })

  it.each([false, true])(
    'discards an obsolete in-flight conversation page after a gap (already reconciled: %s)',
    (alreadyReconciled) => {
      const previous = { id: 'run', before: '100', messages: messages(100, 199), events: [] }
      const live = page(messages(401, 500), '401')
      const loaded = prependConversationPage(
        alreadyReconciled ? reconcileConversationHistory(previous, live) : previous,
        page(messages(0, 99)),
        live,
        '100',
      )
      expect(loaded.messages).toEqual(live.messages)
      expect(loaded.before).toBe('401')
    },
  )

  it('keeps overlapping history and replaces a small provisional tail without resetting it', () => {
    const previous = { id: 'run', before: null, messages: messages(0, 199), events: [] }
    const continuous = reconcileConversationHistory(previous, page(messages(150, 249), '150'))
    expect(continuous.before).toBeNull()
    expect(continuous.messages).toEqual(messages(0, 249))
    const streaming = {
      id: 'run',
      before: null,
      messages: [message('earlier'), message('stream', 'streaming')],
      events: [],
    }
    const settled = reconcileConversationHistory(streaming, page([message('final')]))
    expect(settled.messages).toEqual([message('earlier'), message('final')])
    expect(settled.before).toBeNull()
    // A window with only a provisional response has no canonical history gap.
    expect(
      reconcileConversationHistory(
        { ...streaming, messages: [message('stream', 'streaming')] },
        page([message('final')], 'final'),
      ).before,
    ).toBeNull()
  })

  it('does not let a late pending-result refresh recreate a disconnected message range', () => {
    const previous = {
      id: 'run',
      before: null,
      messages: [...messages(0, 198), message('199', 'pending')],
      events: [],
    }
    const live = page(messages(401, 500), '401')
    const refreshed = refreshConversationHistory(previous, [message('199')], live)
    expect(refreshed.messages).toEqual(live.messages)
    expect(refreshed.before).toBe('401')
  })

  it('resets a disconnected timeline and restores complete cursor-based access', () => {
    const live = {
      ...page([]),
      events: events(1000, 1299),
      history: { ...page([]).history!, events_before: 1000 },
    }
    let loaded = reconcileTimelineHistory(
      { id: 'run', source: false, before: null, events: events(0, 599) },
      live,
    )
    expect(loaded.events).toEqual(live.events)
    expect(loaded.before).toBe(1000)
    for (let count = 0; loaded.before !== null && count < 10; count++) {
      const cursor = loaded.before
      const start = Math.max(0, cursor - 300)
      loaded = prependTimelinePage(
        loaded,
        { events: events(start, cursor - 1), before: start > 0 ? start : null },
        live,
        cursor,
      )
    }
    expect(loaded.before).toBeNull()
    expect(loaded.events.map((event) => event.sequence)).toEqual(range(0, 1299))
  })

  it.each([false, true])(
    'discards an obsolete in-flight timeline page after a gap (already reconciled: %s)',
    (alreadyReconciled) => {
      const previous = { id: 'run', source: false, before: 300, events: events(300, 599) }
      const live = {
        ...page([]),
        events: events(1000, 1299),
        history: { ...page([]).history!, events_before: 1000 },
      }
      const loaded = prependTimelinePage(
        alreadyReconciled ? reconcileTimelineHistory(previous, live) : previous,
        { events: events(0, 299), before: null },
        live,
        300,
      )
      expect(loaded.events).toEqual(live.events)
      expect(loaded.before).toBe(1000)
    },
  )

  it('keeps memory-source timeline history independent from the current live tail', () => {
    const source = { id: 'run', source: true, before: 300, events: events(300, 599) }
    const live = {
      ...page([]),
      events: events(1000, 1299),
      history: { ...page([]).history!, events_before: 1000 },
    }
    expect(reconcileTimelineHistory(source, live)).toBe(source)
    expect(
      prependTimelinePage(source, { events: events(0, 299), before: null }, live, 300),
    ).toEqual({ ...source, before: null, events: events(0, 599) })
  })
})

describe('loaded conversation history reconciliation', () => {
  it('refreshes pending rows that came from the live tail before sliding out of it', () => {
    const saved = [message('older'), message('command', 'pending'), message('after')]
    expect(hiddenPendingMessages(saved, saved.slice(1))).toEqual([])
    const live = [message('after'), message('new-result')]
    const retained = mergeMessageHistory(saved, live)
    expect(hiddenPendingMessages(retained, live)).toEqual([{ id: 'command', before: 'after' }])
    const settled = refreshHistoryMessages(retained, [message('command')], live)
    expect(settled.map((item) => item.id)).toEqual(['older', 'command', 'after', 'new-result'])
    expect(hiddenPendingMessages(settled, live)).toEqual([])
  })

  it.each(['complete', 'interrupted'] as const)(
    'replaces temporary stream rows with a %s canonical reply',
    (status) => {
      const saved = [message('older'), message('stream-10', 'streaming')]
      const final: ChatMessage = { ...message('event-11', status), role: 'assistant' }
      expect(mergeMessageHistory(saved, [final])).toEqual([saved[0], final])
      expect(mergeMessageHistory(saved, [])).toEqual([saved[0]])
    },
  )

  it('updates known rows without inserting unknown page contents or regressing newer live results', () => {
    const pending = message('command', 'pending')
    const saved = [message('older'), pending, message('later')]
    const stale = [message('unknown'), { ...pending, text: 'old request' }]
    const final = { ...message('command'), text: 'verified output' }
    const result = refreshHistoryMessages(saved, stale, [final, message('latest')])
    expect(result.map((item) => item.id)).toEqual(['older', 'command', 'later', 'latest'])
    expect(result[1]).toEqual(final)
    expect(refreshHistoryMessages(result, stale, [message('latest')])[1]).toEqual(final)
  })

  it('uses stable canonical cursors and skips pending rows still supplied by live snapshots', () => {
    const live = [message('live-command', 'pending'), message('stream-20', 'streaming')]
    const all = [message('hidden', 'pending'), ...live]
    expect(hiddenPendingMessages(all, live)).toEqual([{ id: 'hidden', before: 'live-command' }])
  })
})

describe('history pending-state refresh', () => {
  it('reads one page for adjacent pending rows rather than one request per command', async () => {
    const rows = [
      message('a', 'pending'),
      message('b', 'pending'),
      message('c', 'pending'),
      message('end'),
    ]
    const read = vi
      .fn()
      .mockResolvedValue(page(rows.slice(0, 3).map((item) => ({ ...item, status: 'complete' }))))
    const apply = vi.fn()
    await refreshPendingMessages(
      'run',
      hiddenPendingMessages(rows, [rows[3]]),
      read,
      apply,
      () => true,
    )
    expect(read).toHaveBeenCalledTimes(1)
    expect(read).toHaveBeenCalledWith('run', 'end')
    expect(apply).toHaveBeenCalledTimes(1)
    expect(apply.mock.calls[0][0].every((item: ChatMessage) => item.status === 'complete')).toBe(
      true,
    )
  })

  it('walks across a skipped live-page gap to find an old command result', async () => {
    const read = vi
      .fn()
      .mockResolvedValueOnce(page([message('gap-start'), message('gap-end')], 'gap-start'))
      .mockResolvedValueOnce(page([message('old-command')]))
    const apply = vi.fn()
    await refreshPendingMessages(
      'run',
      [{ id: 'old-command', before: 'new-tail' }],
      read,
      apply,
      () => true,
    )
    expect(read.mock.calls).toEqual([
      ['run', 'new-tail'],
      ['run', 'gap-start'],
    ])
    expect(apply.mock.calls[1][0]).toEqual([message('old-command')])
  })

  it('ignores an in-flight response after switching sessions or restarting the refresh effect', async () => {
    let resolve!: (value: RunDetail) => void
    let active = true
    const read = vi.fn(
      () =>
        new Promise<RunDetail>((done) => {
          resolve = done
        }),
    )
    const apply = vi.fn()
    const refresh = refreshPendingMessages(
      'run',
      [{ id: 'old', before: 'next' }],
      read,
      apply,
      () => active,
    )
    active = false
    resolve(page([message('old')]))
    await refresh
    expect(apply).not.toHaveBeenCalled()
  })

  it('propagates refresh errors without pretending a pending command completed', async () => {
    const apply = vi.fn()
    await expect(
      refreshPendingMessages(
        'run',
        [{ id: 'old', before: 'next' }],
        vi.fn().mockRejectedValue(new Error('offline')),
        apply,
        () => true,
      ),
    ).rejects.toThrow('offline')
    expect(apply).not.toHaveBeenCalled()
  })

  it('terminates defensively when a server repeats the same page cursor', async () => {
    const read = vi.fn().mockResolvedValue(page([message('unrelated')], 'next'))
    await refreshPendingMessages('run', [{ id: 'old', before: 'next' }], read, vi.fn(), () => true)
    expect(read).toHaveBeenCalledTimes(1)
  })
})

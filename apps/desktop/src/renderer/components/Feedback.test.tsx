import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it, vi } from 'vitest'
import { ErrorState, InlineError } from './Feedback'

describe('runtime recovery messages', () => {
  it('keeps reconnect available for a transient connection error', () => {
    const html = renderToStaticMarkup(
      <ErrorState error={new Error('Connection interrupted')} retry={vi.fn()} />,
    )
    expect(html).toContain('重新连接')
  })

  it('does not offer reconnect for a stopped runtime that requires an app restart', () => {
    const message = 'The local runtime stopped (0). 请重新启动 Open Harness。'
    const full = renderToStaticMarkup(<ErrorState error={new Error(message)} retry={vi.fn()} />)
    const inline = renderToStaticMarkup(<InlineError message={message} retry={vi.fn()} />)
    for (const html of [full, inline]) {
      expect(html).toContain('请重新启动 Open Harness')
      expect(html).not.toContain('<button')
    }
  })
})

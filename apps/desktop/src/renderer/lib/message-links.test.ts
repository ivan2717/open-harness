import { describe, expect, it } from 'vitest'
import { messageLink } from './message-links'

describe('message links', () => {
  it('keeps local file references separate from network URLs', () => {
    expect(messageLink('/project/报告.md:12')).toEqual({
      kind: 'local',
      source: '/project/报告.md:12',
    })
    expect(messageLink('./my%20report.csv')).toEqual({ kind: 'local', source: './my%20report.csv' })
    for (const source of ['app.ts:12', 'app.ts:12:3', 'app.ts%3A12'])
      expect(messageLink(source)).toEqual({ kind: 'local', source })
    expect(messageLink('C:\\project\\report.md')).toEqual({
      kind: 'local',
      source: 'C:\\project\\report.md',
    })
    expect(messageLink('https://example.com/clip.mp4?download=1')).toMatchObject({
      kind: 'external',
      media: 'video',
    })
    expect(messageLink('mailto:user@example.com')).toEqual({
      kind: 'external',
      url: 'mailto:user@example.com',
    })
  })

  it.each([
    'javascript:alert(1)',
    'javascript:12',
    'data:image/svg+xml,<svg/>',
    'file:///etc/passwd',
    'blob:https://example.com/x',
    '//example.com/secret',
    'https://user:pass@example.com',
    'java\nscript:alert(1)',
    'javascript%3Aalert(1)',
    '%00file.txt',
    'broken%path',
  ])('rejects unsafe URL %s', (value) => {
    expect(messageLink(value).kind).toBe('blocked')
  })
})

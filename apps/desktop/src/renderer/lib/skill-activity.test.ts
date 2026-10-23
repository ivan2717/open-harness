import { describe, expect, it } from 'vitest'
import type { ChatMessage } from '../../shared/contracts'
import { skillActivityLabel } from './skill-activity'

const loading: ChatMessage = {
  id: 'tool-load-review',
  role: 'tool',
  tool_name: 'load_skill',
  status: 'pending',
  text: JSON.stringify({ id: 'review' }),
}

function loaded(result: Record<string, unknown>): ChatMessage {
  return { ...loading, status: 'complete', text: JSON.stringify(result) }
}

describe('skill activity in the conversation', () => {
  it('reports actual loading and successful output separately from requested skills', () => {
    expect(skillActivityLabel(loading)).toBe('加载技能 · review')
    expect(
      skillActivityLabel(
        loaded({
          id: 'review',
          name: 'Review',
          content: 'Instructions',
          offset: 0,
          next_offset: null,
        }),
      ),
    ).toBe('已加载技能 · Review')
    expect(skillActivityLabel({ ...loading, role: 'user', text: '$review' })).toBeNull()
    expect(skillActivityLabel({ ...loading, role: 'assistant', text: 'Using review' })).toBeNull()
    expect(skillActivityLabel({ ...loading, tool_name: 'list_skills' })).toBeNull()
  })

  it('keeps namespaced plugin IDs and tolerates legacy output without exposing instruction bodies', () => {
    expect(skillActivityLabel(loaded({ id: 'docs:review', content: 'Private instructions' }))).toBe(
      '已加载技能 · docs:review',
    )
    expect(skillActivityLabel({ ...loading, status: 'complete', text: 'Legacy result' })).toBe(
      '技能读取已完成',
    )
    expect(
      skillActivityLabel(loaded({ content: 'Do not put these instructions in the title' })),
    ).toBe('已加载技能')
  })

  it('does not call failed loads or paginated fragments a fully loaded skill', () => {
    expect(
      skillActivityLabel({ ...loading, status: 'complete', is_error: true, text: 'Missing' }),
    ).toBe('技能加载失败')
    expect(
      skillActivityLabel(
        loaded({ id: 'long-skill', content: 'First part', offset: 0, next_offset: 4096 }),
      ),
    ).toBe('已读取技能片段 · long-skill')
    expect(
      skillActivityLabel(
        loaded({ id: 'long-skill', content: 'Last part', offset: 4096, next_offset: null }),
      ),
    ).toBe('已读取技能片段 · long-skill')
  })
})

import { describe, expect, it } from 'vitest'
import { insertSkillMention } from './skill-mention'

describe('skill mention insertion', () => {
  it('keeps plugin namespaces and leaves the caret ready to type', () => {
    expect(insertSkillMention('', 'org.review:review', 0, 0)).toEqual({
      text: '$org.review:review ',
      cursor: 19,
    })
  })
  it('inserts at the caret without joining adjacent text', () => {
    expect(insertSkillMention('请检查这份文件', 'review', 3, 3)).toEqual({
      text: '请检查 $review 这份文件',
      cursor: 12,
    })
  })
  it('replaces only the selected range and preserves surrounding whitespace', () => {
    expect(insertSkillMention('Use old\nthen continue', 'review', 4, 7)).toEqual({
      text: 'Use $review\nthen continue',
      cursor: 11,
    })
  })
})

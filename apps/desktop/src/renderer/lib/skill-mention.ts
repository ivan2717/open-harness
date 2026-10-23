/** Insert an explicit skill request into this message, preserving the selected text position. */
export function insertSkillMention(draft: string, id: string, start: number, end: number) {
  const from = Math.max(0, Math.min(start, draft.length))
  const to = Math.max(from, Math.min(end, draft.length))
  const before = draft.slice(0, from)
  const after = draft.slice(to)
  const mention = `${before && !/\s$/.test(before) ? ' ' : ''}$${id}${!after || !/^\s/.test(after) ? ' ' : ''}`
  return { text: before + mention + after, cursor: before.length + mention.length }
}

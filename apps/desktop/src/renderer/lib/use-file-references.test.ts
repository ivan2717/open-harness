import { describe, expect, it } from 'vitest'
import type { ImageFileReference } from '../../shared/contracts'
import { mergeDraftReferences } from './use-file-references'

function image(name: string, value = name): ImageFileReference {
  return { name, image: { mime_type: 'image/png', data_base64: btoa(value) } }
}

describe('mixed attachment drafts', () => {
  it('deduplicates pasted pixels even when the clipboard gives them different names', () => {
    const original = { ...image('first.png', 'same pixels'), id: 'keep' }
    const merged = mergeDraftReferences([original], {
      files: [image('second.png', 'same pixels'), image('other.png')],
      errors: [],
    })
    expect(merged.files.map((file) => file.name)).toEqual(['first.png', 'other.png'])
    expect(merged.files[0].id).toBe('keep')
    expect(merged.errors).toEqual([])
  })

  it('counts text separately from images and preserves accepted attachments on overflow', () => {
    const existing = [{ name: 'notes.txt', text: 'x'.repeat(512 * 1024), id: 'text' }]
    const merged = mergeDraftReferences(existing, {
      files: [image('screen.png'), { name: 'extra.txt', text: 'too much' }],
      errors: [],
    })
    expect(merged.files.map((file) => file.name)).toEqual(['notes.txt', 'screen.png'])
    expect(merged.errors).toHaveLength(1)
    expect(merged.errors[0]).toContain('512 KB')
  })

  it('counts original-file metadata separately and deduplicates identical named attachments', () => {
    const file = { name: 'large.pdf', attachment: { id: 'a'.repeat(64), size: 90 * 1024 * 1024 } }
    const merged = mergeDraftReferences([{ ...file, id: 'keep' }], {
      files: [file, { ...file, name: 'other.pdf' }],
      errors: [],
    })
    expect(merged.files.map((file) => file.name)).toEqual(['large.pdf', 'other.pdf'])
    expect(merged.files[0].id).toBe('keep')
    expect(merged.errors).toEqual([])
  })

  it('enforces image and total limits across successive imports', () => {
    const existing = Array.from({ length: 4 }, (_, index) => ({
      ...image(`${index}.png`),
      id: `${index}`,
    }))
    const merged = mergeDraftReferences(existing, {
      files: [
        image('fifth.png'),
        ...Array.from({ length: 61 }, (_, index) => ({ name: `${index}.txt`, text: 'text' })),
      ],
      errors: [],
    })
    expect(merged.files).toHaveLength(64)
    expect(merged.errors).toEqual(['每条消息最多添加 4 张图片。', '每条消息最多添加 64 个附件。'])
  })
})

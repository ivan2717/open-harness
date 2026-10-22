import { isFileLocation } from '../../shared/message-resources'

export type MessageLink =
  | { kind: 'external'; url: string; media?: 'image' | 'video' | 'audio' }
  | { kind: 'local'; source: string }
  | { kind: 'blocked' }

/** The renderer only classifies links; the main process enforces file authority. */
export function messageLink(value: string): MessageLink {
  const source = value.trim()
  if (!source || /[\u0000-\u001f\u007f]/.test(source) || source.startsWith('//'))
    return { kind: 'blocked' }
  if (/^https?:\/\//i.test(source)) {
    try {
      const url = new URL(source)
      if (url.username || url.password) return { kind: 'blocked' }
      const extension = url.pathname.split('.').at(-1)?.toLowerCase()
      const media = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'avif'].includes(extension ?? '')
        ? 'image'
        : ['mp4', 'webm', 'mov', 'm4v', 'ogv'].includes(extension ?? '')
          ? 'video'
          : ['mp3', 'wav', 'ogg', 'm4a', 'flac', 'aac'].includes(extension ?? '')
            ? 'audio'
            : undefined
      return { kind: 'external', url: url.href, ...(media ? { media } : {}) }
    } catch {
      return { kind: 'blocked' }
    }
  }
  if (/^mailto:[^\s]+$/i.test(source)) return { kind: 'external', url: source }
  if (source.startsWith('#')) return { kind: 'blocked' }
  // Windows drive paths are local paths, not URL schemes.
  if (/^[a-z]:[\\/]/i.test(source)) return { kind: 'local', source }
  let decoded: string
  try {
    decoded = decodeURIComponent(source)
  } catch {
    return { kind: 'blocked' }
  }
  if (
    (/^[a-z][a-z\d+.-]*:/i.test(decoded) && !isFileLocation(decoded)) ||
    /[\u0000-\u001f\u007f]/.test(decoded)
  )
    return { kind: 'blocked' }
  return { kind: 'local', source }
}

export function resourceName(source: string): string {
  try {
    const path = /^https?:/i.test(source) ? new URL(source).pathname : source
    return decodeURIComponent(path.split(/[\\/]/).at(-1) || source)
  } catch {
    return source
  }
}

import {
  MAX_IMAGE_REFERENCE_BYTES,
  MAX_IMAGE_REFERENCE_PIXELS,
  MAX_IMAGE_REFERENCE_EDGE,
} from '../shared/file-references'

export type ImageMimeType = 'image/png' | 'image/jpeg' | 'image/webp' | 'image/gif'
export interface ImageDimensions {
  width: number
  height: number
  mimeType: ImageMimeType
}

/** Inspect dimensions before native decoding so small compressed inputs cannot allocate unbounded pixels. */
export function imageDimensions(bytes: Uint8Array): ImageDimensions {
  const data = Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  let width = 0
  let height = 0
  let mimeType: ImageMimeType
  if (
    data.length >= 24 &&
    data.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])) &&
    data.toString('ascii', 12, 16) === 'IHDR'
  ) {
    width = data.readUInt32BE(16)
    height = data.readUInt32BE(20)
    mimeType = 'image/png'
  } else if (data.length >= 10 && /^GIF8[79]a$/.test(data.toString('ascii', 0, 6))) {
    width = data.readUInt16LE(6)
    height = data.readUInt16LE(8)
    mimeType = 'image/gif'
  } else if (
    data.length >= 30 &&
    data.toString('ascii', 0, 4) === 'RIFF' &&
    data.toString('ascii', 8, 12) === 'WEBP'
  ) {
    mimeType = 'image/webp'
    const chunk = data.toString('ascii', 12, 16)
    if (chunk === 'VP8X') {
      width = 1 + data.readUIntLE(24, 3)
      height = 1 + data.readUIntLE(27, 3)
    } else if (chunk === 'VP8L' && data[20] === 0x2f) {
      const dimensions = data.readUInt32LE(21)
      width = (dimensions & 0x3fff) + 1
      height = ((dimensions >>> 14) & 0x3fff) + 1
    } else if (chunk === 'VP8 ' && data[23] === 0x9d && data[24] === 0x01 && data[25] === 0x2a) {
      width = data.readUInt16LE(26) & 0x3fff
      height = data.readUInt16LE(28) & 0x3fff
    }
  } else if (data.length >= 4 && data[0] === 0xff && data[1] === 0xd8) {
    mimeType = 'image/jpeg'
    let offset = 2
    while (offset + 4 <= data.length) {
      if (data[offset++] !== 0xff) break
      while (offset < data.length && data[offset] === 0xff) offset++
      const marker = data[offset++]
      if (marker === 0xda || marker === 0xd9) break
      if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) continue
      if (offset + 2 > data.length) break
      const size = data.readUInt16BE(offset)
      if (size < 2 || offset + size > data.length) break
      if (
        [0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf].includes(
          marker,
        ) &&
        size >= 8
      ) {
        height = data.readUInt16BE(offset + 3)
        width = data.readUInt16BE(offset + 5)
        break
      }
      offset += size
    }
  } else {
    throw new Error('不支持此图片格式，请使用 PNG、JPEG、WebP 或 GIF。')
  }
  if (!width || !height || !Number.isSafeInteger(width * height))
    throw new Error('图片数据无效或已损坏。')
  return { width, height, mimeType }
}

/** Validate a persisted/provider-bound snapshot without accepting arbitrary base64 content. */
export function imageSnapshotBytes(value: unknown): {
  bytes: Buffer
  mimeType: 'image/png' | 'image/jpeg'
} {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('图片快照无效。')
  const image = value as Record<string, unknown>
  if (
    !['image/png', 'image/jpeg'].includes(String(image.mime_type)) ||
    typeof image.data_base64 !== 'string' ||
    image.data_base64.length > Math.ceil(MAX_IMAGE_REFERENCE_BYTES / 3) * 4
  )
    throw new Error('图片格式或大小无效。')
  const bytes = Buffer.from(image.data_base64, 'base64')
  if (
    !bytes.length ||
    bytes.length > MAX_IMAGE_REFERENCE_BYTES ||
    bytes.toString('base64') !== image.data_base64
  )
    throw new Error('图片编码或大小无效。')
  const dimensions = imageDimensions(bytes)
  if (
    dimensions.mimeType !== image.mime_type ||
    dimensions.width > MAX_IMAGE_REFERENCE_EDGE ||
    dimensions.height > MAX_IMAGE_REFERENCE_EDGE ||
    dimensions.width * dimensions.height > MAX_IMAGE_REFERENCE_PIXELS
  )
    throw new Error('图片格式或尺寸无效。')
  return { bytes, mimeType: image.mime_type as 'image/png' | 'image/jpeg' }
}

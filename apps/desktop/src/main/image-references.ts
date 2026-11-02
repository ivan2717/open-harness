import { extname } from 'node:path'
import { nativeImage } from 'electron'
import type { FileImportResult, ImageFileReference } from '../shared/contracts'
import {
  MAX_IMAGE_REFERENCE_BYTES,
  MAX_IMAGE_REFERENCE_FILES,
  MAX_IMAGE_REFERENCE_PIXELS,
  MAX_IMAGE_REFERENCE_EDGE,
  MAX_IMAGE_SOURCE_BYTES,
} from '../shared/file-references'
import { imageDimensions } from './image-validation'

export const IMAGE_EXTENSIONS = new Set(['.png', '.jpg', '.jpeg', '.webp', '.gif'])
const MAX_SOURCE_PIXELS = 32_000_000

function imageName(value: unknown, extension: string): string {
  if (
    typeof value !== 'string' ||
    !value.trim() ||
    [...value].length > 255 ||
    /[\/\\\u0000-\u001f\u007f-\u009f]/.test(value)
  )
    throw new Error('图片文件名称无效。')
  const originalExtension = extname(value)
  const stem = originalExtension ? value.slice(0, -originalExtension.length) : value
  let name = ''
  for (const character of stem) {
    if (Buffer.byteLength(name + character + extension, 'utf8') > 255) break
    name += character
  }
  return (name || '图片') + extension
}

/** Decode and re-encode user-selected pixels; no active formats or metadata are forwarded. */
export function imageReference(name: string, value: Uint8Array): ImageFileReference {
  imageName(name, '.png')
  if (!(value instanceof Uint8Array) || !value.length || value.length > MAX_IMAGE_SOURCE_BYTES)
    throw new Error('图片超过 32 MiB 或内容为空，请选择较小的图片。')
  const source = Buffer.from(value.buffer, value.byteOffset, value.byteLength)
  const declared = imageDimensions(source)
  if (declared.width * declared.height > MAX_SOURCE_PIXELS)
    throw new Error('图片尺寸过大，请先缩小图片。')
  let image = nativeImage.createFromBuffer(source)
  if (image.isEmpty()) throw new Error('图片无法解码，请重新复制或导出图片。')
  const size = image.getSize()
  if (!size.width || !size.height || size.width * size.height > MAX_SOURCE_PIXELS)
    throw new Error('图片尺寸过大或无法读取。')
  const ratio = Math.min(
    1,
    Math.sqrt(MAX_IMAGE_REFERENCE_PIXELS / (size.width * size.height)),
    MAX_IMAGE_REFERENCE_EDGE / size.width,
    MAX_IMAGE_REFERENCE_EDGE / size.height,
  )
  if (ratio < 1) {
    image = image.resize({
      width: Math.max(1, Math.floor(size.width * ratio)),
      height: Math.max(1, Math.floor(size.height * ratio)),
      quality: 'best',
    })
  }
  let bytes = image.toPNG()
  let mime_type: 'image/png' | 'image/jpeg' = 'image/png'
  if (bytes.length > MAX_IMAGE_REFERENCE_BYTES) {
    mime_type = 'image/jpeg'
    bytes = image.toJPEG(90)
    if (bytes.length > MAX_IMAGE_REFERENCE_BYTES) bytes = image.toJPEG(75)
  }
  if (!bytes.length || bytes.length > MAX_IMAGE_REFERENCE_BYTES)
    throw new Error('图片处理后仍超过 5 MiB，请先缩小图片。')
  return {
    name: imageName(name, mime_type === 'image/png' ? '.png' : '.jpg'),
    image: { mime_type, data_base64: bytes.toString('base64') },
  }
}

export function importImages(value: unknown): FileImportResult {
  if (!Array.isArray(value) || value.length > MAX_IMAGE_REFERENCE_FILES)
    throw new Error('每条消息最多添加 4 张图片。')
  const result: FileImportResult = { files: [], errors: [] }
  for (const item of value) {
    try {
      if (!item || typeof item !== 'object' || Array.isArray(item))
        throw new Error('图片数据无效。')
      result.files.push(imageReference(item.name, item.bytes))
    } catch (error) {
      result.errors.push(error instanceof Error ? error.message : '无法读取图片。')
    }
  }
  return result
}

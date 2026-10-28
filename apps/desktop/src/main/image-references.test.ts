import { beforeEach, describe, expect, it, vi } from 'vitest'
import { nativeImage } from 'electron'
import { imageReference, importImages } from './image-references'
import { imageDimensions, imageSnapshotBytes } from './image-validation'
import {
  MAX_IMAGE_REFERENCE_BYTES,
  MAX_IMAGE_SOURCE_BYTES,
  MAX_IMAGE_REFERENCE_PIXELS,
  MAX_IMAGE_REFERENCE_EDGE,
} from '../shared/file-references'

vi.mock('electron', () => ({ nativeImage: { createFromBuffer: vi.fn() } }))
const png = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jq1sAAAAASUVORK5CYII=',
  'base64',
)

function decodedImage(width = 1, height = 1) {
  const result = {
    isEmpty: () => false,
    getSize: () => ({ width, height }),
    resize: vi.fn(),
    toPNG: vi.fn(() => png),
    toJPEG: vi.fn(() => Buffer.from([0xff, 0xd8, 0xff, 0xd9])),
  }
  result.resize.mockReturnValue(result)
  return result
}

beforeEach(() => {
  vi.clearAllMocks()
  vi.mocked(nativeImage.createFromBuffer).mockReturnValue(decodedImage() as never)
})

describe('clipboard and selected image snapshots', () => {
  it('decodes and normalizes an image from memory without requiring a path', () => {
    const result = imageReference('截图.webp', new Uint8Array(png))
    expect(result).toEqual({
      name: '截图.png',
      image: { mime_type: 'image/png', data_base64: png.toString('base64') },
    })
    expect(nativeImage.createFromBuffer).toHaveBeenCalledWith(png)
    expect(result).not.toHaveProperty('text')
    expect(result).not.toHaveProperty('path')
  })

  it('scales a 4K screenshot proportionally to the accepted pixel budget', () => {
    const decoded = decodedImage(3840, 2160)
    vi.mocked(nativeImage.createFromBuffer).mockReturnValue(decoded as never)
    imageReference('screenshot.png', png)
    const resize = decoded.resize.mock.calls[0][0]
    expect(resize.width * resize.height).toBeLessThanOrEqual(MAX_IMAGE_REFERENCE_PIXELS)
    expect(resize.width / resize.height).toBeCloseTo(3840 / 2160, 2)
    expect(resize.quality).toBe('best')
  })

  it('scales narrow panoramas to the maximum edge even below the pixel budget', () => {
    for (const [width, height] of [
      [100, 20_000],
      [20_000, 100],
    ]) {
      const decoded = decodedImage(width, height)
      vi.mocked(nativeImage.createFromBuffer).mockReturnValue(decoded as never)
      imageReference('panorama.png', png)
      const resize = decoded.resize.mock.calls[0][0]
      expect(Math.max(resize.width, resize.height)).toBe(MAX_IMAGE_REFERENCE_EDGE)
      expect(resize.width * resize.height).toBeLessThanOrEqual(MAX_IMAGE_REFERENCE_PIXELS)
    }
  })

  it('uses a bounded JPEG snapshot when normalized PNG bytes exceed the upload budget', () => {
    const decoded = decodedImage()
    decoded.toPNG.mockReturnValue(Buffer.alloc(MAX_IMAGE_REFERENCE_BYTES + 1))
    vi.mocked(nativeImage.createFromBuffer).mockReturnValue(decoded as never)
    const result = imageReference('photo.png', png)
    expect(result.name).toBe('photo.jpg')
    expect(result.image.mime_type).toBe('image/jpeg')
    expect(decoded.toJPEG).toHaveBeenCalledWith(90)
  })

  it('rejects oversized sources, invalid formats, decompression bombs and decode failures', () => {
    expect(() => imageReference('big.png', new Uint8Array(MAX_IMAGE_SOURCE_BYTES + 1))).toThrow(
      '32 MiB',
    )
    expect(() => imageReference('vector.svg', Buffer.from('<svg/>'))).toThrow('格式')
    const bomb = Buffer.from(png)
    bomb.writeUInt32BE(100_000, 16)
    bomb.writeUInt32BE(100_000, 20)
    expect(() => imageReference('bomb.png', bomb)).toThrow('尺寸过大')
    expect(nativeImage.createFromBuffer).not.toHaveBeenCalled()
    vi.mocked(nativeImage.createFromBuffer).mockReturnValue({ isEmpty: () => true } as never)
    expect(() => imageReference('broken.png', png)).toThrow('无法解码')
  })

  it('validates IPC payloads and retains successful siblings', () => {
    const result = importImages([
      { name: 'ok.png', bytes: png },
      { name: '../bad.png', bytes: png },
      { name: 'raw.png', bytes: 'not bytes' },
    ])
    expect(result.files).toHaveLength(1)
    expect(result.errors).toHaveLength(2)
    expect(() => importImages(Array(5).fill({ name: 'a.png', bytes: png }))).toThrow('4 张')
    expect(() => importImages({ name: 'a.png', bytes: png })).toThrow('4 张')
    expect(
      Buffer.byteLength(imageReference(`${'汉'.repeat(83)}.png`, png).name),
    ).toBeLessThanOrEqual(255)
  })
})

describe('image snapshot validation', () => {
  it('accepts canonical PNG bytes and rejects misleading MIME, malformed base64 and oversized dimensions', () => {
    const snapshot = { mime_type: 'image/png', data_base64: png.toString('base64') }
    expect(imageSnapshotBytes(snapshot)).toEqual({ bytes: png, mimeType: 'image/png' })
    for (const image of [
      { ...snapshot, mime_type: 'image/jpeg' },
      { ...snapshot, data_base64: snapshot.data_base64 + '\n' },
      { ...snapshot, data_base64: Buffer.from('<svg/>').toString('base64') },
      { ...snapshot, data_base64: Buffer.alloc(MAX_IMAGE_REFERENCE_BYTES + 1).toString('base64') },
    ])
      expect(() => imageSnapshotBytes(image)).toThrow()
    const large = Buffer.from(png)
    large.writeUInt32BE(3000, 16)
    large.writeUInt32BE(2000, 20)
    expect(() =>
      imageSnapshotBytes({ ...snapshot, data_base64: large.toString('base64') }),
    ).toThrow('尺寸')
    for (const [width, height] of [
      [100, 20_000],
      [20_000, 100],
    ]) {
      const panorama = Buffer.from(png)
      panorama.writeUInt32BE(width, 16)
      panorama.writeUInt32BE(height, 20)
      expect(() =>
        imageSnapshotBytes({ ...snapshot, data_base64: panorama.toString('base64') }),
      ).toThrow('尺寸')
    }
  })

  it('recognizes the dimensions of each accepted source container before decoding', () => {
    expect(imageDimensions(png)).toEqual({ width: 1, height: 1, mimeType: 'image/png' })
    const gif = Buffer.from('GIF89a\x03\x00\x02\x00', 'binary')
    expect(imageDimensions(gif)).toEqual({ width: 3, height: 2, mimeType: 'image/gif' })
    const webp = Buffer.alloc(30)
    webp.write('RIFF', 0)
    webp.write('WEBPVP8X', 8)
    webp.writeUIntLE(2, 24, 3)
    webp.writeUIntLE(1, 27, 3)
    expect(imageDimensions(webp)).toEqual({ width: 3, height: 2, mimeType: 'image/webp' })
    const jpeg = Buffer.from([0xff, 0xd8, 0xff, 0xc0, 0, 8, 8, 0, 2, 0, 3, 0])
    expect(imageDimensions(jpeg)).toEqual({ width: 3, height: 2, mimeType: 'image/jpeg' })
  })
})

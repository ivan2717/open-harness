import CoreGraphics

// A small target-local check, not a whole-window animation detector. These
// tolerances absorb minor capture/rendering noise; they are not proof that the
// action is semantically safe. Window/control validation still runs afterwards.
enum VisualGuard {
    static func matches(_ before: CGImage, _ after: CGImage, point: CGPoint, windowSize: CGSize) -> Bool {
        guard before.width == after.width, before.height == after.height,
              windowSize.width > 0, windowSize.height > 0,
              point.x.isFinite, point.y.isFinite, point.x >= 0, point.y >= 0,
              point.x < windowSize.width, point.y < windowSize.height else { return false }
        let x = Int(point.x * Double(before.width) / windowSize.width)
        let y = Int(point.y * Double(before.height) / windowSize.height)
        let left = max(0, x - 10), top = max(0, y - 10)
        let width = min(before.width, x + 11) - left
        let height = min(before.height, y + 11) - top
        let rect = CGRect(x: left, y: top, width: width, height: height)
        guard let old = pixels(before, rect), let new = pixels(after, rect) else { return false }
        var changed = 0
        for offset in stride(from: 0, to: old.count, by: 4) {
            if (0..<3).contains(where: { abs(Int(old[offset + $0]) - Int(new[offset + $0])) > 24 }) { changed += 1 }
        }
        return changed * 10 <= width * height
    }

    private static func pixels(_ image: CGImage, _ rect: CGRect) -> [UInt8]? {
        guard let cropped = image.cropping(to: rect), let space = CGColorSpace(name: CGColorSpace.sRGB) else { return nil }
        let width = Int(rect.width), height = Int(rect.height)
        var bytes = [UInt8](repeating: 0, count: width * height * 4)
        let rendered = bytes.withUnsafeMutableBytes { buffer -> Bool in
            guard let context = CGContext(data: buffer.baseAddress, width: width, height: height,
                                          bitsPerComponent: 8, bytesPerRow: width * 4, space: space,
                                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue) else { return false }
            context.draw(cropped, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        return rendered ? bytes : nil
    }
}

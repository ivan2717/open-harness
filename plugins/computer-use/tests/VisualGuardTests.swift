import CoreGraphics
import Foundation

@main enum VisualGuardTests {
    static func main() throws {
        let cases = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))) as! [[String: Any]]
        for test in cases {
            func number(_ key: String) -> Int { test[key] as! Int }
            func image(_ after: Bool) -> CGImage {
                let width = after ? test["after_width"] as? Int ?? number("width") : number("width")
                let height = number("height")
                var pixels = [UInt8](repeating: 100, count: width * height * 4)
                for offset in stride(from: 3, to: pixels.count, by: 4) { pixels[offset] = 255 }
                if after {
                    for patch in test["patches"] as! [[Int]] {
                        for y in patch[1]..<min(height, patch[1] + patch[3]) {
                            for x in patch[0]..<min(width, patch[0] + patch[2]) {
                                let offset = (y * width + x) * 4
                                for channel in 0..<3 { pixels[offset + channel] = UInt8(patch[4]) }
                                if patch.count > 5 { pixels[offset] = UInt8(patch[5]) }
                            }
                        }
                    }
                }
                let provider = CGDataProvider(data: Data(pixels) as CFData)!
                return CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
                               space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue),
                               provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
            }
            let actual = VisualGuard.matches(image(false), image(true), point: CGPoint(x: number("x"), y: number("y")), windowSize: CGSize(width: number("window_width"), height: number("window_height")))
            guard actual == test["matches"] as! Bool else { fatalError("Visual guard failed: \(test["name"]!)") }
        }
        print("Passed \(cases.count) visual guard cases")
    }
}

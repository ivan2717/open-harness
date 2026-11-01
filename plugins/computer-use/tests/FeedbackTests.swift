import AppKit
import ScreenCaptureKit

// Capture and virtual cursor confined to windows owned by this disposable test.
@main struct FeedbackTests {
    @MainActor static func main() {
        do { try recoveryPolicy() }
        catch { fputs("\(error)\n", stderr); exit(1) }
        if CommandLine.arguments.contains("--recovery-only") {
            print("Passed macOS preview recovery policy"); return
        }
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        Task { @MainActor in
            do { try await run(); print("Passed macOS feedback GUI acceptance"); exit(0) }
            catch { fputs("\(error)\n", stderr); exit(1) }
        }
        app.run()
    }
    struct Failure: Error { let message: String }
    @MainActor static func check(_ condition: @autoclosure () -> Bool, _ message: String) throws {
        if !condition() { throw Failure(message: message) }
    }
    @MainActor static func until(_ test: () -> Bool, _ message: String) async throws {
        for _ in 0..<100 {
            if test() { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw Failure(message: message)
    }
    @MainActor static func recoveryPolicy() throws {
        var recovery = PreviewRecovery(), now = 0.0
        try check(recovery.ready(at: now), "Initial capture must be allowed")
        for delay in [1.0, 2.0, 4.0, 8.0, 8.0] {
            recovery.fail(at: now)
            try check(!recovery.ready(at: now + delay - 0.01), "Retry must respect backoff")
            now += delay
            try check(recovery.ready(at: now), "Transient capture must become retryable")
        }
        recovery.fail(at: now)
        try check(!recovery.ready(at: now + 3600), "Persistent failure must stop automatic retries")
        recovery.reset()
        try check(recovery.ready(at: now) && !recovery.failed, "An explicit observation or successful frame must reset recovery")
    }
    @MainActor static func run() async throws {
        try check(CGPreflightScreenCaptureAccess(), "Screen recording permission is required for this GUI test; it is not requested automatically.")
        let source = NSWindow(contentRect: CGRect(x: 200, y: 220, width: 600, height: 360), styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        source.title = "Open Harness feedback fixture"; source.isReleasedWhenClosed = false
        source.contentView?.wantsLayer = true
        source.contentView?.layer?.backgroundColor = NSColor(calibratedRed: 0.94, green: 0.96, blue: 0.93, alpha: 1).cgColor
        let label = NSTextField(labelWithString: "Computer Use · 实时窗口预览")
        label.frame = CGRect(x: 32, y: 260, width: 550, height: 40); label.font = .systemFont(ofSize: 24)
        source.contentView?.addSubview(label)
        let counter = NSTextField(labelWithString: "本地画面，不发送给模型")
        counter.frame = CGRect(x: 32, y: 210, width: 550, height: 40); counter.font = .systemFont(ofSize: 16)
        source.contentView?.addSubview(counter)
        var ticks = 0
        let animation = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { _ in
            MainActor.assumeIsolated { ticks += 1; counter.stringValue = "本地画面，不发送给模型 · \(ticks)" }
        }
        source.orderFrontRegardless()
        let preview = FeedbackPreview(), cursor = FeedbackCursor()
        defer { animation.invalidate(); preview.stop(); cursor.reset(); source.close() }
        let process = try ProcessIdentity.read(getpid())
        let identity = AppIdentity(process: process, bundle: "dev.openharness.feedback-test", bundlePath: "/invalid-test-bundle")
        let binding = WindowBinding(element: nil, id: CGWindowID(source.windowNumber), owner: process)
        func frame() throws -> CGRect {
            guard let info = windowInfo().first(where: { ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == binding.id }), let frame = cgBounds(info) else {
                throw Failure(message: "Missing test window")
            }
            return frame
        }
        // Fault injection uses only this fixture's window. A transient capture
        // error must recover without a second observe or a collapse/expand.
        var attempts = 0
        let recovering = FeedbackPreview(captureImage: { filter, config in
            attempts += 1
            if attempts == 1 { throw Failure(message: "Injected transient capture failure") }
            return try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
        })
        recovering.setEnabled(true)
        recovering.observe(identity, binding, frame: try frame(), continuous: false)
        try await until({ recovering.framesReceived > 0 }, "Transient screenshot failure stayed latched")
        recovering.stop()
        var clock = 0.0, failures = 0
        var succeed = false
        let exhausted = FeedbackPreview(captureImage: { filter, config in
            if !succeed { failures += 1; throw Failure(message: "Injected persistent capture failure") }
            return try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
        }, now: { clock })
        exhausted.setEnabled(true)
        exhausted.observe(identity, binding, frame: try frame(), continuous: false)
        try await until({ failures == 1 }, "Initial failing capture did not run")
        for count in 2...6 {
            clock += 10
            try await until({ failures == count }, "Bounded retry did not run")
        }
        clock += 100
        try await Task.sleep(for: .milliseconds(650))
        try check(failures == 6, "Exhausted capture kept retrying")
        succeed = true
        exhausted.observe(identity, binding, frame: try frame(), continuous: false)
        try await until({ exhausted.framesReceived > 0 }, "Observe did not recover the same target after retry exhaustion")
        exhausted.stop()
        // A cancelled native call can complete late. Do not start another call
        // until it returns, and never publish its frame under the new target.
        var pending: [CheckedContinuation<CGImage, Error>] = []
        let delayed = FeedbackPreview(captureImage: { _, _ in
            try await withCheckedThrowingContinuation { pending.append($0) }
        })
        delayed.setEnabled(true)
        delayed.observe(identity, binding, frame: try frame(), continuous: false)
        try await until({ pending.count == 1 }, "Delayed capture did not start")
        delayed.stop()
        delayed.observe(identity, binding, frame: try frame(), continuous: false)
        try await Task.sleep(for: .milliseconds(650))
        try check(pending.count == 1, "Cancelled capture stacked another native operation")
        let pixels = CGContext(data: nil, width: 8, height: 8, bitsPerComponent: 8, bytesPerRow: 32, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!.makeImage()!
        pending[0].resume(returning: pixels)
        try await until({ pending.count == 2 }, "New observation did not resume after old capture returned")
        try check(delayed.framesReceived == 0, "Late frame from stopped observation was published")
        delayed.stop(); pending[1].resume(returning: pixels)
        let foreground = NSWorkspace.shared.frontmostApplication?.processIdentifier
        preview.setEnabled(true)
        preview.observe(identity, binding, frame: try frame())
        try check(NSWorkspace.shared.frontmostApplication?.processIdentifier == foreground, "Preview stole focus")
        try await until({ preview.framesReceived > 2 }, "Real preview stream did not deliver frames")
        try check(preview.status["local_only"] as? Bool == true, "Frames must remain local")
        let pointer = NSEvent.mouseLocation
        let pointerForeground = NSWorkspace.shared.frontmostApplication?.processIdentifier
        cursor.show(local: CGPoint(x: 180, y: 180), frame: try frame(), windowID: binding.id, pressed: true)
        preview.pointer(CGPoint(x: 180, y: 180), pressed: true)
        try check(pointer == NSEvent.mouseLocation, "Feedback moved the system pointer")
        try check(NSWorkspace.shared.frontmostApplication?.processIdentifier == pointerForeground, "Cursor stole focus")
        // The person may keep using their mouse during async capture/rendering.
        try await Task.sleep(for: .milliseconds(250))
        try check(!NSApp.windows.contains(where: { $0.title == "Open Harness · 操作预览" }), "Capture must not create a native preview window")
        preview.setEnabled(false)
        try check(preview.status["enabled"] as? Bool == false && preview.status["streaming"] as? Bool == false, "Collapse must release capture")
        let stoppedFrames = preview.framesReceived
        try await Task.sleep(for: .milliseconds(200))
        try check(preview.framesReceived == stoppedFrames, "Collapsed preview still receives frames")
        preview.setEnabled(true)
        try await until({ preview.framesReceived > stoppedFrames + 1 }, "Expand failed to restart capture")
        source.setContentSize(CGSize(width: 480, height: 480))
        let beforeResize = preview.framesReceived
        try await until({ preview.framesReceived > beforeResize + 2 }, "Resize failed to preserve live capture")
        preview.observe(identity, binding, frame: try frame(), continuous: false)
        try await until({ preview.framesReceived > 2 }, "AX-less preview failed to refresh local screenshots")
        try check(preview.status["capture_mode"] as? String == "snapshots", "AX-less preview mode")
        // A native sharing decoration must not make a sole canvas window look
        // like multiple keyboard destinations. Never waive input validation.
        try await until({ !windowInfo().contains { ($0[kCGWindowOwnerPID as String] as? Int32) == getpid() && ($0[kCGWindowName as String] as? String) == "Window" && ($0[kCGWindowIsOnscreen as String] as? Bool) == true } }, "Preview retained a sharing decoration on the canvas window")
        source.close()
        try await until({ preview.status["active"] as? Bool == false }, "Closed source left stale preview visible")
        try check(preview.status["streaming"] as? Bool == false, "Closed source retained stream")
        source.orderFrontRegardless()
        preview.observe(identity, binding, frame: try frame())
        // Stop with an async capture start in flight; it must never reappear.
        preview.stop()
        try await Task.sleep(for: .milliseconds(500))
        try check(preview.status["active"] as? Bool == false && preview.status["streaming"] as? Bool == false, "Late capture start resurrected stopped preview")
    }
}

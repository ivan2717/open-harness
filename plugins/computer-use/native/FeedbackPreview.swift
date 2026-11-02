import AppKit
import CoreImage
import ScreenCaptureKit

// A transient capture error must not require restarting the app. Retry a small
// number of times, then wait for an explicit new observation/enable operation.
struct PreviewRecovery {
    private(set) var failures = 0
    private(set) var retryAt: TimeInterval?
    var failed: Bool { failures > 0 }
    mutating func reset() { failures = 0; retryAt = nil }
    mutating func fail(at now: TimeInterval) {
        failures += 1
        retryAt = failures <= 5 ? now + min(8, pow(2, Double(failures - 1))) : nil
    }
    func ready(at now: TimeInterval) -> Bool { !failed || retryAt.map { now >= $0 } == true }
}

// Capture only. Presentation belongs to the Electron renderer, never a native
// preview window. These transient frames are intercepted before MCP output.
@MainActor final class FeedbackPreview: NSObject, SCStreamOutput, SCStreamDelegate {
    static var emit: ([String: Any]) -> Void = { _ in }
    private var stream: SCStream?
    // Keep cancelled tasks until they actually return: cancellation does not
    // necessarily interrupt ScreenCaptureKit, so clearing early stacks work.
    private var captureTask: Task<Void, Never>?
    private var captureStop: Task<Void, Never>?
    private var continuous = true
    private var generation = 0
    private var enabled = false
    private var binding: WindowBinding?
    private var identity: AppIdentity?
    private var monitor: Timer?
    private var frameSize = CGSize(width: 1, height: 1)
    private var recovery = PreviewRecovery()
    private var title = "电脑操作"
    private var lastFrameTime = 0.0
    private var pointerSequence = 0
    private let images = CIContext(options: [.cacheIntermediates: false])
    private let captureImage: (SCContentFilter, SCStreamConfiguration) async throws -> CGImage
    private let now: () -> TimeInterval
    init(captureImage: @escaping (SCContentFilter, SCStreamConfiguration) async throws -> CGImage = {
        try await SCScreenshotManager.captureImage(contentFilter: $0, configuration: $1)
    }, now: @escaping () -> TimeInterval = CACurrentMediaTime) {
        self.captureImage = captureImage; self.now = now; super.init()
    }
    private(set) var framesReceived = 0
    var status: [String: Any] {
        ["in_app": true, "active": binding != nil, "enabled": enabled,
         "window_id": binding.map { Int($0.id) } as Any? ?? NSNull(),
         "streaming": binding != nil && enabled && !recovery.failed && (stream != nil || captureTask?.isCancelled == false), "capture_mode": continuous ? "video" : "snapshots",
         "frames_received": framesReceived, "local_only": true]
    }
    private func send(_ extra: [String: Any] = [:]) {
        guard binding != nil else { Self.emit(["active": false]); return }
        var value: [String: Any] = ["active": true, "title": title, "width": frameSize.width, "height": frameSize.height]
        value.merge(extra) { _, new in new }; Self.emit(value)
    }
    func setEnabled(_ enabled: Bool) {
        guard enabled != self.enabled else { return }
        self.enabled = enabled
        recovery.reset()
        if enabled, binding != nil { startCapture(frameSize) }
        else { endCapture() }
    }
    func observe(_ identity: AppIdentity, _ binding: WindowBinding, frame: CGRect, continuous: Bool = true) {
        let changed = self.identity != identity || self.binding?.id != binding.id || self.binding?.owner != binding.owner || self.continuous != continuous
        let resized = frameSize != frame.size
        if changed { stop(); framesReceived = 0 }
        self.continuous = continuous; self.identity = identity; self.binding = binding; frameSize = frame.size
        title = String((NSRunningApplication(processIdentifier: identity.pid)?.localizedName ?? "电脑操作").prefix(128))
        send()
        if changed { startMonitor() }
        if recovery.failed && recovery.retryAt == nil { recovery.reset() }
        if enabled && (changed || resized) { startCapture(frame.size) }
        else { startWhenReady() }
    }
    func pointer(_ point: CGPoint, pressed: Bool) {
        pointerSequence += 1
        send(["pointer": ["x": point.x, "y": point.y, "pressed": pressed, "sequence": pointerSequence]])
    }
    private func startMonitor() {
        monitor = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, let identity = self.identity, let binding = self.binding else { return }
                guard (try? requireUnlocked()) != nil, (try? identity.process.validate()) != nil,
                      (try? binding.owner.validate()) != nil, CGPreflightScreenCaptureAccess(),
                      let info = windowInfo().first(where: { ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == binding.id }),
                      (info[kCGWindowOwnerPID as String] as? Int) == Int(binding.owner.pid),
                      (info[kCGWindowIsOnscreen as String] as? Bool) == true, let frame = cgBounds(info), frame.width > 1, frame.height > 1 else {
                    self.stop(); return
                }
                if self.frameSize != frame.size {
                    self.frameSize = frame.size; self.send()
                    if self.enabled { self.startCapture(frame.size) }
                } else { self.startWhenReady() }
            }
        }
    }
    private func endCapture() {
        generation += 1; captureTask?.cancel()
        let old = stream; stream = nil
        if let old {
            captureStop = Task { [weak self] in
                try? await old.stopCapture()
                self?.captureStop = nil
            }
        }
    }
    private func configuration(_ size: CGSize) -> SCStreamConfiguration {
        let config = SCStreamConfiguration(), scale = min(1, 960 / max(size.width, size.height))
        config.width = max(1, Int(size.width * scale)); config.height = max(1, Int(size.height * scale))
        config.minimumFrameInterval = CMTime(value: 1, timescale: 6); config.queueDepth = 3
        config.showsCursor = false; config.capturesAudio = false; config.ignoreShadowsSingleWindow = true
        return config
    }
    private func startCapture(_ size: CGSize) {
        endCapture(); frameSize = size; recovery.reset(); lastFrameTime = 0
        startWhenReady()
    }
    private func startWhenReady() {
        guard enabled, let binding, stream == nil, captureTask == nil, captureStop == nil,
              recovery.ready(at: now()), CGPreflightScreenCaptureAccess() else { return }
        let size = frameSize
        let revision = generation
        if !continuous { startSnapshots(binding, size, revision); return }
        captureTask = Task { @MainActor [weak self] in
            guard let self else { return }
            defer { self.captureTask = nil }
            do {
                let available = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: false)
                guard !Task.isCancelled, revision == self.generation, self.enabled else { return }
                guard let window = available.windows.first(where: { $0.windowID == binding.id && $0.owningApplication?.processID == binding.owner.pid }) else { self.unavailable(); return }
                let capture = SCStream(filter: SCContentFilter(desktopIndependentWindow: window), configuration: self.configuration(size), delegate: self)
                try capture.addStreamOutput(self, type: .screen, sampleHandlerQueue: .main)
                do { try await capture.startCapture() }
                catch { try? await capture.stopCapture(); throw error }
                guard !Task.isCancelled, revision == self.generation, self.enabled else { try? await capture.stopCapture(); return }
                self.stream = capture
            } catch { if !Task.isCancelled && revision == self.generation { self.unavailable() } }
        }
    }
    private func startSnapshots(_ binding: WindowBinding, _ size: CGSize, _ revision: Int) {
        // Continuous capture adds a sharing decoration which can make AX-less
        // keyboard routing ambiguous. Keep the existing screenshot fallback.
        captureTask = Task { @MainActor [weak self] in
            guard let self else { return }
            defer { self.captureTask = nil }
            do {
                let available = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: false)
                guard let window = available.windows.first(where: { $0.windowID == binding.id && $0.owningApplication?.processID == binding.owner.pid }) else { throw ComputerError("WINDOW_CHANGED", "Preview source closed.") }
                let filter = SCContentFilter(desktopIndependentWindow: window), config = self.configuration(size)
                while !Task.isCancelled && revision == self.generation && self.enabled {
                    let image = try await self.captureImage(filter, config)
                    guard !Task.isCancelled, revision == self.generation else { return }
                    self.frame(image); try await Task.sleep(for: .milliseconds(250))
                }
            } catch { if !Task.isCancelled && revision == self.generation { self.unavailable() } }
        }
    }
    private func frame(_ image: CGImage) {
        guard enabled, binding != nil else { return }
        let bitmap = NSBitmapImageRep(cgImage: image)
        guard let data = bitmap.representation(using: .jpeg, properties: [.compressionFactor: 0.7]), data.count <= 600_000 else { unavailable(); return }
        recovery.reset(); framesReceived += 1; send(["jpeg": data.base64EncodedString()])
    }
    private func unavailable() { endCapture(); recovery.fail(at: now()); send(["unavailable": true]) }
    nonisolated func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        MainActor.assumeIsolated {
            guard stream === self.stream, type == .screen, sampleBuffer.isValid, self.enabled,
                  CACurrentMediaTime() - lastFrameTime >= 0.15,
                  let info = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
                  let status = info.first?[.status] as? Int, status == SCFrameStatus.complete.rawValue,
                  let pixels = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
            lastFrameTime = CACurrentMediaTime()
            let source = CIImage(cvPixelBuffer: pixels)
            if let image = images.createCGImage(source, from: source.extent) { frame(image) }
        }
    }
    nonisolated func stream(_ stream: SCStream, didStopWithError error: Error) {
        Task { @MainActor [weak self] in if let self, stream === self.stream { self.unavailable() } }
    }
    func stop() {
        monitor?.invalidate(); monitor = nil; endCapture()
        identity = nil; binding = nil; recovery.reset(); send()
    }
}

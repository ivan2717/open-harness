import AppKit
import QuartzCore

// A visual indicator only. It never moves the system pointer or receives input.
final class FeedbackCursorView: NSView {
    var pressed = false { didSet { needsDisplay = true } }
    var pressProgress: CGFloat = 1 { didSet { needsDisplay = true } }
    override var isFlipped: Bool { true }
    override func draw(_ dirtyRect: NSRect) {
        let center = CGPoint(x: 25, y: 24)
        let glow = NSGradient(colorsAndLocations: (NSColor(calibratedWhite: 0.25, alpha: 0.20), 0),
                              (NSColor(calibratedWhite: 0.35, alpha: 0.08), 0.45), (.clear, 1))
        glow?.draw(in: NSBezierPath(ovalIn: CGRect(x: 0, y: 0, width: 58, height: 58)), relativeCenterPosition: .zero)
        if pressed && pressProgress < 1 {
            NSColor(calibratedWhite: 0.55, alpha: 0.32 * (1 - pressProgress)).setStroke()
            let radius = 5 + 15 * pressProgress
            let ring = NSBezierPath(ovalIn: CGRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2))
            ring.lineWidth = 1; ring.stroke()
        }
        let arrow = NSBezierPath()
        arrow.move(to: center)
        arrow.line(to: CGPoint(x: center.x + 2, y: center.y + 22))
        arrow.line(to: CGPoint(x: center.x + 7, y: center.y + 16))
        arrow.line(to: CGPoint(x: center.x + 11, y: center.y + 23))
        arrow.line(to: CGPoint(x: center.x + 14, y: center.y + 21))
        arrow.line(to: CGPoint(x: center.x + 10, y: center.y + 14))
        arrow.line(to: CGPoint(x: center.x + 18, y: center.y + 12))
        arrow.close()
        NSGraphicsContext.saveGraphicsState()
        let shadow = NSShadow(); shadow.shadowColor = NSColor.black.withAlphaComponent(0.22)
        shadow.shadowBlurRadius = 4; shadow.shadowOffset = CGSize(width: 0, height: -1); shadow.set()
        NSColor(calibratedWhite: 0.48, alpha: 0.9).setFill(); arrow.fill()
        NSGraphicsContext.restoreGraphicsState()
        NSColor.white.withAlphaComponent(0.95).setStroke(); arrow.lineWidth = 1.3; arrow.lineJoinStyle = .round; arrow.stroke()
    }
}

@MainActor final class FeedbackCursor {
    private let panel: NSPanel
    private let view = FeedbackCursorView(frame: CGRect(x: 0, y: 0, width: 64, height: 64))
    private var timer: Timer?
    private var lastPoint: CGPoint?
    private var windowID: CGWindowID?

    init() {
        panel = NSPanel(contentRect: view.bounds, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.isOpaque = false; panel.backgroundColor = .clear; panel.hasShadow = false
        panel.ignoresMouseEvents = true; panel.hidesOnDeactivate = false
        panel.collectionBehavior = [.transient, .ignoresCycle, .fullScreenAuxiliary]
        panel.contentView = view
        panel.sharingType = .none
    }

    func show(local: CGPoint, frame: CGRect, windowID: CGWindowID, pressed: Bool) {
        timer?.invalidate()
        let destination = CGPoint(x: frame.minX + local.x, y: frame.minY + local.y)
        let start = self.windowID == windowID ? lastPoint ?? destination : destination
        self.windowID = windowID; lastPoint = destination
        view.pressed = pressed; view.pressProgress = 0
        let began = CACurrentMediaTime()
        place(start)
        // Stay with the target window rather than floating above unrelated apps.
        panel.order(.above, relativeTo: Int(windowID))
        timer = Timer.scheduledTimer(withTimeInterval: 1 / 60, repeats: true) { [weak self] timer in
            MainActor.assumeIsolated {
                guard let self else { timer.invalidate(); return }
                let elapsed = CACurrentMediaTime() - began
                self.view.pressProgress = min(1, elapsed / 0.35)
                let t = min(1, elapsed / 0.16), eased = 1 - pow(1 - t, 3)
                self.place(CGPoint(x: start.x + (destination.x - start.x) * eased, y: start.y + (destination.y - start.y) * eased))
                self.panel.alphaValue = CGFloat(max(0, min(1, (1.5 - elapsed) / 0.3)))
                if elapsed > 1.5 { self.hide() }
            }
        }
        panel.alphaValue = 1
    }

    private func place(_ point: CGPoint) {
        let desktopTop = NSScreen.screens.first?.frame.maxY ?? 0
        panel.setFrameOrigin(CGPoint(x: point.x - 25, y: desktopTop - point.y - 40))
    }

    func hide() { timer?.invalidate(); timer = nil; panel.orderOut(nil) }
    func reset() { hide(); lastPoint = nil; windowID = nil }
}

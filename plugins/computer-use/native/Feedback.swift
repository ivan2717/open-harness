import AppKit

// One feedback surface per helper/MCP session. No extra permission prompts,
// capture targets or model-visible tools are introduced by the presentation.
@MainActor final class VisualFeedback {
    static let shared = VisualFeedback()
    private var preview: FeedbackPreview?
    private var cursor: FeedbackCursor?
    private var enabled = false
    var status: [String: Any] { preview?.status ?? ["in_app": true, "active": false, "local_only": true] }
    func setEnabled(_ enabled: Bool) { self.enabled = enabled; preview?.setEnabled(enabled) }
    func observe(_ identity: AppIdentity, _ binding: WindowBinding, frame: CGRect) {
        if preview == nil { preview = FeedbackPreview(); preview?.setEnabled(enabled) }
        let app = AXUIElementCreateApplication(binding.owner.pid)
        let focused = try? elementAttribute(app, "AXFocusedWindow")
        let continuous = focused.map { matchesAXWindow($0, binding, frame: frame, candidates: windowInfo()) } ?? false
        preview?.observe(identity, binding, frame: frame, continuous: continuous)
    }
    func pointer(_ local: CGPoint, _ state: Observation, pressed: Bool = true) {
        preview?.pointer(local, pressed: pressed)
        if cursor == nil { cursor = FeedbackCursor() }
        cursor?.show(local: local, frame: state.frame, windowID: state.windowID, pressed: pressed)
    }
    func stop() { preview?.stop(); cursor?.reset() }
}

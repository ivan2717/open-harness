import AppKit
import ApplicationServices
import Foundation
import ScreenCaptureKit
import ImageIO
import UniformTypeIdentifiers

struct Observation {
    let binding: WindowBinding
    let frame: CGRect
    let focused: ElementState?
    let nodes: [[String: Any]]
    let refs: [String: ElementState]
    let truncated: Bool
    let accessibility: String
    var window: AXUIElement? { binding.element }
    var windowID: CGWindowID { binding.id }
    var json: [String: Any] { ["window": ["bounds": rectJSON(frame), "id": Int(windowID), "pid": binding.owner.pid], "elements": nodes, "truncated": truncated, "accessibility": accessibility, "next": nodes.isEmpty ? "Use the window screenshot and coordinates; this window does not expose readable accessibility controls." : "Use observed refs or screenshot coordinates, then observe to verify."] }
    func withFrame(_ frame: CGRect) -> Observation { Observation(binding: binding, frame: frame, focused: focused, nodes: nodes, refs: refs, truncated: truncated, accessibility: accessibility) }
}
func focusedElement(_ identity: AppIdentity, _ binding: WindowBinding) throws -> AXUIElement? {
    guard let window = binding.element else { return nil }
    // App-local focus can stay valid while another application is frontmost.
    // Never consult the system-wide AXFocusedApplication or follow a new window.
    let app = AXUIElementCreateApplication(binding.owner.pid)
    if let item = try elementAttribute(app, "AXFocusedUIElement"),
       (try? elementOwner(item, identity)) != nil, try belongsToWindow(item, window) { return item }
    return nil
}
func observe(_ identity: AppIdentity, _ binding: WindowBinding) throws -> Observation {
    let frame = try validateWindow(identity, binding)
    // Refresh optional AX information without changing the selected owner/ID.
    // Inconsistent AX geometry disables refs, not the window screenshot.
    let binding = binding.withElement(accessibleWindow(identity, binding, frame: frame))
    // Focus is optional for reference/coordinate actions. A transient focus
    // failure must not make every otherwise-readable control unobservable.
    let candidateFocus = try? focusedElement(identity, binding)
    var focused: ElementState?
    var nodes: [[String: Any]] = []; var refs: [String: ElementState] = [:]; var seen: [AXUIElement] = []; var truncated = false
    let deadline = Date().addingTimeInterval(6)
    func walk(_ item: AXUIElement, _ depth: Int, protectedAncestor: Bool = false) throws {
        if seen.contains(where: { CFEqual($0, item) }) { return }
        guard nodes.count < 250, depth < 16, Date() < deadline else { truncated = true; return }
        seen.append(item)
        guard (try? elementOwner(item, identity)) != nil else { truncated = true; return }
        let rawState = try ElementState.read(item, identity, frame: frame)
        let protected = protectedAncestor || rawState.secure
        // Store exactly the capabilities emitted in this observation. Hidden
        // actions must not remain invocable by guessing their names later.
        let state = protected ? rawState.redacted() : rawState
        let ref = "e\(nodes.count)"
        var node: [String: Any] = ["ref": ref, "role": state.role, "depth": depth, "actions": protected ? [] : state.actions, "settable_value": state.valueSettable && !protected]
        if !state.subrole.isEmpty { node["subrole"] = state.subrole }
        for key in ["AXTitle", "AXDescription", "AXIdentifier", "AXEnabled", "AXFocused"] {
            if let value = try attribute(item, key, optionalMetadata: ["AXTitle", "AXDescription", "AXIdentifier"].contains(key)) {
                if let text = value as? String { node[key] = String(text.prefix(400)) }
                else if let number = value as? NSNumber { node[key] = number }
            }
        }
        if !protected, let value = try attribute(item, "AXValue", optionalMetadata: true) as? String { node["AXValue"] = String(value.prefix(800)) }
        if let box = state.box { node["bounds"] = rectJSON(box) }
        if let candidateFocus, CFEqual(candidateFocus, item) { focused = state }
        nodes.append(node); refs[ref] = state
        if let children = try attribute(item, "AXChildren") as? [AXUIElement] { for child in children { try walk(child, depth + 1, protectedAncestor: protected) } }
    }
    if let element = binding.element {
        do { try walk(element, 0) }
        catch { truncated = true }
    }
    if focused == nil, let candidateFocus { focused = try? ElementState.read(candidateFocus, identity, frame: frame) }
    let finalFrame = try validateWindow(identity, binding, expectedSize: frame)
    guard sameRect(finalFrame, frame) else { throw ComputerError("STATE_CHANGED", "Bound window moved while its accessibility tree was being read. Observe again.") }
    return Observation(binding: binding, frame: finalFrame, focused: focused, nodes: nodes, refs: refs, truncated: truncated, accessibility: nodes.isEmpty ? "unavailable" : truncated ? "partial" : "available")
}
func screenshot(_ identity: AppIdentity, _ state: Observation) async throws -> CGImage {
    guard CGPreflightScreenCaptureAccess() else { throw ComputerError("SCREEN_RECORDING_PERMISSION_REQUIRED", "Enable Screen Recording for the Open Harness host/helper in macOS System Settings. No permission dialog was requested.") }
    let frame = try validateWindow(identity, state.binding, expectedSize: state.frame)
    let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: false)
    guard let window = content.windows.first(where: { $0.windowID == state.windowID && $0.owningApplication?.processID == state.binding.owner.pid }), sameSize(window.frame, frame) else {
        throw ComputerError("WINDOW_CHANGED", "Bound window changed before capture. Observe again.")
    }
    let filter = SCContentFilter(desktopIndependentWindow: window)
    let config = SCStreamConfiguration()
    let scale = min(1, 1600 / max(frame.width, frame.height))
    config.width = max(1, Int((frame.width * scale).rounded()))
    config.height = max(1, Int((frame.height * scale).rounded()))
    config.showsCursor = false
    config.ignoreShadowsSingleWindow = true
    let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
    _ = try validateWindow(identity, state.binding, expectedSize: frame)
    return image
}
func screenshotJSON(_ image: CGImage) throws -> [String: Any] {
    let data = NSMutableData()
    guard let destination = CGImageDestinationCreateWithData(data as CFMutableData, UTType.png.identifier as CFString, 1, nil) else { throw ComputerError("CAPTURE_FAILED", "PNG encoder unavailable.") }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination), data.length < 4 * 1024 * 1024 else { throw ComputerError("CAPTURE_FAILED", "Screenshot exceeds the image limit.") }
    return ["mimeType": "image/png", "data": (data as Data).base64EncodedString()]
}

func validateVisualTargets(_ points: [CGPoint], _ state: Observation, _ identity: AppIdentity, _ captured: CGImage?) async throws {
    guard let captured else { throw ComputerError("SCREENSHOT_REQUIRED", "Pointer input requires a screenshot in this observation.") }
    // One fresh, window-only capture per action. No PNG encoding, model request,
    // foreground activation or capture of the user's entire desktop is needed.
    let current = try await screenshot(identity, state)
    for point in points {
        guard VisualGuard.matches(captured, current, point: point, windowSize: state.frame.size) else {
            throw ComputerError("VISUAL_TARGET_CHANGED", "The image near the pointer target changed since observation. No input was dispatched. Observe again and choose the target from the new screenshot.")
        }
    }
}

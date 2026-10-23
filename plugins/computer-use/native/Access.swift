import AppKit
import ApplicationServices
import CryptoKit
import Foundation
import ScreenCaptureKit
import ImageIO
import UniformTypeIdentifiers

struct ComputerError: Error {
    let code: String
    let message: String
    init(_ code: String, _ message: String) { self.code = code; self.message = message }
}
struct AppIdentity: Equatable {
    let pid: pid_t
    let bundle: String
    let executable: String
    let launched: Date
    static func read(_ app: NSRunningApplication) throws -> AppIdentity {
        guard let bundle = app.bundleIdentifier, let executable = app.executableURL?.standardizedFileURL.path,
              let launched = app.launchDate, !app.isTerminated else {
            throw ComputerError("APP_UNAVAILABLE", "Application identity is unavailable. Wait for it to finish launching.")
        }
        return AppIdentity(pid: app.processIdentifier, bundle: bundle, executable: executable, launched: launched)
    }
    var json: [String: Any] { ["pid": pid, "bundle_id": bundle] }
    func running() throws -> NSRunningApplication {
        guard let app = NSRunningApplication(processIdentifier: pid), try AppIdentity.read(app) == self else {
            throw ComputerError("APP_CHANGED", "Selected application exited or its process identity changed. Select it again.")
        }
        return app
    }
}
func attribute(_ element: AXUIElement, _ name: String, optionalMetadata: Bool = false) throws -> CFTypeRef? {
    AXUIElementSetMessagingTimeout(element, 0.3)
    var value: CFTypeRef?
    let status = AXUIElementCopyAttributeValue(element, name as CFString, &value)
    if [.attributeUnsupported, .noValue, .notImplemented].contains(status) { return nil }
    // AppKit controls can report AXError.failure for an absent label/description.
    // Only descriptive metadata is optional: window, identity and geometry failures
    // must still stop observation instead of fabricating a safe action target.
    if status == .failure && optionalMetadata { return nil }
    guard status == .success else { throw ComputerError(status == .apiDisabled ? "ACCESSIBILITY_PERMISSION_REQUIRED" : "AX_UNAVAILABLE", "Cannot read accessibility attribute \(name) (\(status.rawValue)).") }
    return value
}
func elementAttribute(_ element: AXUIElement, _ name: String) throws -> AXUIElement? {
    guard let value = try attribute(element, name), CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
    return unsafeDowncast(value, to: AXUIElement.self)
}
func bounds(_ element: AXUIElement) throws -> CGRect? {
    guard let position = try attribute(element, "AXPosition"), let size = try attribute(element, "AXSize"),
          CFGetTypeID(position) == AXValueGetTypeID(), CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
    var point = CGPoint.zero; var dimensions = CGSize.zero
    guard AXValueGetValue(unsafeDowncast(position, to: AXValue.self), .cgPoint, &point),
          AXValueGetValue(unsafeDowncast(size, to: AXValue.self), .cgSize, &dimensions),
          point.x.isFinite, point.y.isFinite, dimensions.width.isFinite, dimensions.height.isFinite else { return nil }
    return CGRect(origin: point, size: dimensions)
}
func rectJSON(_ rect: CGRect) -> [String: Double] { ["x": rect.minX, "y": rect.minY, "width": rect.width, "height": rect.height] }
func requireAX() throws {
    guard AXIsProcessTrusted() else { throw ComputerError("ACCESSIBILITY_PERMISSION_REQUIRED", "Enable Accessibility for the Open Harness host/helper in macOS System Settings, then restart the computer session. No permission dialog was requested.") }
}
struct Observation {
    let window: AXUIElement
    let frame: CGRect
    let windowID: CGWindowID?
    let focused: AXUIElement?
    let nodes: [[String: Any]]
    let refs: [String: AXUIElement]
    let truncated: Bool
    let fingerprint: String
    var json: [String: Any] { ["window": ["bounds": rectJSON(frame), "id": windowID.map { Int($0) } as Any? ?? NSNull()], "elements": nodes, "truncated": truncated] }
}
func observe(_ identity: AppIdentity) throws -> Observation {
    try requireAX()
    _ = try identity.running()
    let app = AXUIElementCreateApplication(identity.pid)
    guard let window = try elementAttribute(app, "AXFocusedWindow"), let frame = try bounds(window), frame.width > 0, frame.height > 0 else {
        throw ComputerError("WINDOW_UNAVAILABLE", "Selected app has no readable focused window. Bring its intended window forward and observe again.")
    }
    var windowOwner: pid_t = 0
    guard AXUIElementGetPid(window, &windowOwner) == .success, windowOwner == identity.pid else {
        throw ComputerError("WINDOW_OUTSIDE_APP", "Focused window belongs to another process. This plugin cannot operate delegated system dialogs.")
    }
    let candidateFocus = try elementAttribute(app, "AXFocusedUIElement")
    var focusOwner: pid_t = 0
    let focused = candidateFocus.flatMap { item in
        AXUIElementGetPid(item, &focusOwner) == .success && focusOwner == identity.pid ? item : nil
    }
    let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
    let candidates = windows.filter { entry in
        guard (entry[kCGWindowOwnerPID as String] as? Int) == Int(identity.pid), (entry[kCGWindowLayer as String] as? Int) == 0,
              let raw = entry[kCGWindowBounds as String] as? [String: Any],
              let cgFrame = CGRect(dictionaryRepresentation: raw as CFDictionary) else { return false }
        return abs(cgFrame.minX - frame.minX) < 1 && abs(cgFrame.minY - frame.minY) < 1 && abs(cgFrame.width - frame.width) < 1 && abs(cgFrame.height - frame.height) < 1
    }
    let windowID = candidates.count == 1 ? (candidates[0][kCGWindowNumber as String] as? NSNumber)?.uint32Value : nil
    var nodes: [[String: Any]] = []; var refs: [String: AXUIElement] = [:]; var seen: [AXUIElement] = []; var truncated = false
    let deadline = Date().addingTimeInterval(6)
    func walk(_ item: AXUIElement, _ depth: Int) throws {
        if seen.contains(where: { CFEqual($0, item) }) { return }
        guard nodes.count < 250, depth < 16, Date() < deadline else { truncated = true; return }
        seen.append(item)
        var owner: pid_t = 0
        guard AXUIElementGetPid(item, &owner) == .success, owner == identity.pid else {
            truncated = true
            return
        }
        let ref = "e\(nodes.count)"
        let role = (try attribute(item, "AXRole") as? String) ?? "AXUnknown"
        let subrole = (try attribute(item, "AXSubrole") as? String) ?? ""
        var node: [String: Any] = ["ref": ref, "role": role, "depth": depth]
        for key in ["AXTitle", "AXDescription", "AXIdentifier", "AXEnabled", "AXFocused"] {
            if let value = try attribute(item, key, optionalMetadata: ["AXTitle", "AXDescription", "AXIdentifier"].contains(key)) {
                if let text = value as? String { node[key] = String(text.prefix(400)) }
                else if let number = value as? NSNumber { node[key] = number }
            }
        }
        if subrole != "AXSecureTextField", let value = try attribute(item, "AXValue", optionalMetadata: true) as? String { node["AXValue"] = String(value.prefix(800)) }
        if let box = try bounds(item) { node["bounds"] = rectJSON(CGRect(x: box.minX - frame.minX, y: box.minY - frame.minY, width: box.width, height: box.height)) }
        nodes.append(node); refs[ref] = item
        if let children = try attribute(item, "AXChildren") as? [AXUIElement] {
            for child in children { try walk(child, depth + 1) }
        }
    }
    try walk(window, 0)
    let fingerprintData = try JSONSerialization.data(withJSONObject: ["frame": rectJSON(frame), "window_id": windowID.map { Int($0) } as Any? ?? NSNull(), "nodes": nodes, "truncated": truncated], options: [.sortedKeys])
    let fingerprint = SHA256.hash(data: fingerprintData).map { String(format: "%02x", $0) }.joined()
    _ = try identity.running()
    return Observation(window: window, frame: frame, windowID: windowID, focused: focused, nodes: nodes, refs: refs, truncated: truncated, fingerprint: fingerprint)
}
func screenshot(_ identity: AppIdentity, _ state: Observation) async throws -> [String: Any] {
    guard CGPreflightScreenCaptureAccess() else { throw ComputerError("SCREEN_RECORDING_PERMISSION_REQUIRED", "Enable Screen Recording for the Open Harness host/helper in macOS System Settings. No permission dialog was requested.") }
    guard let windowID = state.windowID else { throw ComputerError("WINDOW_AMBIGUOUS", "Cannot identify this AX window uniquely for a window-only screenshot.") }
    let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
    guard let window = content.windows.first(where: { $0.windowID == windowID && $0.owningApplication?.processID == identity.pid }), window.frame == state.frame else {
        throw ComputerError("WINDOW_CHANGED", "Window changed before capture. Observe again.")
    }
    let filter = SCContentFilter(desktopIndependentWindow: window)
    let config = SCStreamConfiguration()
    let scale = min(1, 1600 / max(state.frame.width, state.frame.height))
    config.width = max(1, Int((state.frame.width * scale).rounded()))
    config.height = max(1, Int((state.frame.height * scale).rounded()))
    config.showsCursor = false
    config.ignoreShadowsSingleWindow = true
    let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
    _ = try identity.running()
    guard let currentFrame = try bounds(state.window), currentFrame == state.frame else { throw ComputerError("WINDOW_CHANGED", "Window moved during capture. Observe again.") }
    let data = NSMutableData()
    guard let destination = CGImageDestinationCreateWithData(data as CFMutableData, UTType.png.identifier as CFString, 1, nil) else { throw ComputerError("CAPTURE_FAILED", "PNG encoder unavailable.") }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination), data.length < 4 * 1024 * 1024 else { throw ComputerError("CAPTURE_FAILED", "Screenshot exceeds the image limit.") }
    return ["mimeType": "image/png", "data": (data as Data).base64EncodedString(), "width": image.width, "height": image.height]
}

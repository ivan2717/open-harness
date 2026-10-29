import AppKit
import ApplicationServices
import Darwin
import Foundation
import ScreenCaptureKit
import ImageIO
import UniformTypeIdentifiers

struct ComputerError: Error {
    let code: String
    let message: String
    let dispatched: Bool?
    init(_ code: String, _ message: String, dispatched: Bool? = false) { self.code = code; self.message = message; self.dispatched = dispatched }
    var recovery: (recoverable: Bool, next: String) {
        switch code {
        case "APP_NOT_SELECTED", "APP_NOT_FOUND", "APP_CHANGED", "APP_UNAVAILABLE": return (true, "List applications and select the exact running bundle_id and pid again.")
        case "WINDOW_NOT_FOUND": return (true, "Select the app without window_id, call computer_windows, then select an exact window_id.")
        case "WINDOW_NOT_SELECTED", "WINDOW_AMBIGUOUS", "WINDOW_UNAVAILABLE", "WINDOW_CHANGED": return (true, "Call computer_windows, then select_app with an exact window_id.")
        case "ACCESSIBILITY_PERMISSION_REQUIRED", "SCREEN_RECORDING_PERMISSION_REQUIRED": return (false, "Use the host's permission controls, then check computer_status. This tool did not request permission.")
        case "DESKTOP_LOCKED": return (false, "Unlock the desktop before observing or sending input.")
        case "SECURE_INPUT_UNSUPPORTED": return (false, "The user must enter passwords or protected content directly.")
        case "INPUT_CANCELLED": return (false, "Start a new computer session and inspect the target window before continuing.")
        case "INPUT_ROUTING_UNAVAILABLE", "UNSUPPORTED_SYSTEM_SHORTCUT", "ACTION_UNSUPPORTED": return (false, "Use an advertised accessibility action or a supported window-scoped input operation.")
        case "INVALID_REQUEST", "UNKNOWN_METHOD": return (false, "Correct the request to match the supported computer tool schema.")
        default: return (true, "Observe the bound window before choosing another action.")
        }
    }
}

// proc_bsdinfo supplies an OS process birth time even for applications whose
// NSRunningApplication.launchDate is nil (including some embedded app hosts).
// PID, executable, UID and birth time must all survive every input boundary.
struct ProcessIdentity: Equatable {
    let pid: pid_t
    let executable: String
    let uid: uid_t
    let startedSeconds: UInt64
    let startedMicroseconds: UInt64
    static func read(_ pid: pid_t) throws -> ProcessIdentity {
        var info = proc_bsdinfo()
        let count = Int32(MemoryLayout.size(ofValue: info))
        var path = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
        guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, count) == count,
              proc_pidpath(pid, &path, UInt32(path.count)) > 0, info.pbi_start_tvsec > 0 else {
            throw ComputerError("APP_CHANGED", "Process exited or its start identity cannot be verified. Select the application again.")
        }
        let executable = URL(fileURLWithPath: String(cString: path)).resolvingSymlinksInPath().standardizedFileURL.path
        return ProcessIdentity(pid: pid, executable: executable, uid: info.pbi_uid, startedSeconds: info.pbi_start_tvsec, startedMicroseconds: info.pbi_start_tvusec)
    }
    func validate() throws {
        guard try Self.read(pid) == self else { throw ComputerError("APP_CHANGED", "Process identity changed. No further input was dispatched.") }
    }
}
struct AppIdentity: Equatable {
    let process: ProcessIdentity
    let bundle: String
    let bundlePath: String
    var pid: pid_t { process.pid }
    static func read(_ app: NSRunningApplication) throws -> AppIdentity {
        guard let bundle = app.bundleIdentifier, let root = app.bundleURL, !app.isTerminated else {
            throw ComputerError("APP_UNAVAILABLE", "Application bundle identity is unavailable. Wait for it to finish launching.")
        }
        let process = try ProcessIdentity.read(app.processIdentifier)
        let bundlePath = root.resolvingSymlinksInPath().standardizedFileURL.path
        guard process.executable.hasPrefix(bundlePath + "/"), process.uid == getuid() else {
            throw ComputerError("APP_UNAVAILABLE", "Application executable does not belong to its declared bundle and current user.")
        }
        return AppIdentity(process: process, bundle: bundle, bundlePath: bundlePath)
    }
    var json: [String: Any] { ["pid": pid, "bundle_id": bundle] }
    @discardableResult func running() throws -> NSRunningApplication {
        guard let app = NSRunningApplication(processIdentifier: pid), try AppIdentity.read(app) == self else {
            throw ComputerError("APP_CHANGED", "Selected application exited or its process identity changed. Select it again.")
        }
        return app
    }
    func trustedProcess(_ owner: pid_t) throws -> ProcessIdentity {
        if owner == pid { try process.validate(); return process }
        let candidate = try ProcessIdentity.read(owner)
        // XPC/LaunchServices may launch a same-bundle AX helper under launchd,
        // so PPID ancestry is not authoritative. Trust only this exact canonical
        // app directory and UID, then retain the helper's OS birth identity.
        // Every actionable element must also belong to the bound AX window.
        guard candidate.uid == process.uid, candidate.executable.hasPrefix(bundlePath + "/") else {
            throw ComputerError("TARGET_OUTSIDE_APP", "Accessibility target is not a trusted helper inside the selected app bundle.")
        }
        try process.validate()
        return candidate
    }
}
func attribute(_ element: AXUIElement, _ name: String, optionalMetadata: Bool = false) throws -> CFTypeRef? {
    AXUIElementSetMessagingTimeout(element, 0.3)
    var value: CFTypeRef?
    let status = AXUIElementCopyAttributeValue(element, name as CFString, &value)
    if [.attributeUnsupported, .noValue, .notImplemented].contains(status) { return nil }
    if status == .failure && optionalMetadata { return nil }
    guard status == .success else { throw ComputerError(status == .apiDisabled ? "ACCESSIBILITY_PERMISSION_REQUIRED" : "AX_UNAVAILABLE", "Cannot read accessibility attribute \(name) (\(status.rawValue)).") }
    return value
}
func elementAttribute(_ element: AXUIElement, _ name: String) throws -> AXUIElement? {
    guard let value = try attribute(element, name), CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
    return unsafeDowncast(value, to: AXUIElement.self)
}
func elementOwner(_ element: AXUIElement, _ identity: AppIdentity) throws -> ProcessIdentity {
    var pid: pid_t = 0
    guard AXUIElementGetPid(element, &pid) == .success else { throw ComputerError("TARGET_UNAVAILABLE", "Cannot verify accessibility target ownership.") }
    return try identity.trustedProcess(pid)
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
func sameSize(_ a: CGRect, _ b: CGRect) -> Bool { abs(a.width - b.width) < 0.5 && abs(a.height - b.height) < 0.5 }
func sameRect(_ a: CGRect, _ b: CGRect) -> Bool { sameSize(a, b) && abs(a.minX - b.minX) < 1 && abs(a.minY - b.minY) < 1 }
func requireAX() throws {
    guard AXIsProcessTrusted() else { throw ComputerError("ACCESSIBILITY_PERMISSION_REQUIRED", "Enable Accessibility for the Open Harness host/helper in macOS System Settings, then restart the computer session. No permission dialog was requested.") }
}
func requireUnlocked() throws {
    guard let session = CGSessionCopyCurrentDictionary() as? [String: Any],
          (session[kCGSessionOnConsoleKey] as? Bool) == true,
          (session[kCGSessionLoginDoneKey] as? Bool) == true,
          (session["CGSSessionScreenIsLocked"] as? Bool) != true,
          NSWorkspace.shared.frontmostApplication?.bundleIdentifier != "com.apple.loginwindow" else {
        throw ComputerError("DESKTOP_LOCKED", "The desktop is locked or unavailable. No input was dispatched.")
    }
}
func windowInfo() -> [[String: Any]] {
    CGWindowListCopyWindowInfo([.optionAll, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
}
func cgBounds(_ entry: [String: Any]) -> CGRect? {
    guard let raw = entry[kCGWindowBounds as String] as? [String: Any] else { return nil }
    return CGRect(dictionaryRepresentation: raw as CFDictionary)
}
func axWindowID(_ window: AXUIElement) -> CGWindowID? {
    typealias GetWindow = @convention(c) (AXUIElement, UnsafeMutablePointer<CGWindowID>) -> Int32
    guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "_AXUIElementGetWindow") else { return nil }
    var id: CGWindowID = 0
    return unsafeBitCast(symbol, to: GetWindow.self)(window, &id) == 0 && id != 0 ? id : nil
}
struct WindowBinding {
    let element: AXUIElement
    let id: CGWindowID
    let owner: ProcessIdentity
}
struct AppWindow {
    let binding: WindowBinding
    let frame: CGRect
    let title: String
    let minimized: Bool
    var json: [String: Any] { ["id": Int(binding.id), "pid": binding.owner.pid, "title": title, "bounds": rectJSON(frame), "minimized": minimized] }
}
func applicationWindows(_ identity: AppIdentity) throws -> [AppWindow] {
    try requireAX(); try identity.running()
    let app = AXUIElementCreateApplication(identity.pid)
    let windows = try attribute(app, "AXWindows") as? [AXUIElement] ?? []
    let serverWindows = windowInfo()
    var result: [AppWindow] = []
    for window in windows {
        guard let owner = try? elementOwner(window, identity), let frame = try? bounds(window), frame.width > 0, frame.height > 0 else { continue }
        let explicitID = axWindowID(window)
        let candidates = serverWindows.filter { entry in
            guard (entry[kCGWindowOwnerPID as String] as? Int) == Int(owner.pid),
                  let id = (entry[kCGWindowNumber as String] as? NSNumber)?.uint32Value else { return false }
            // Exact AX window numbers remain authoritative while a window is
            // moving/resizing. Frame matching is only an identity fallback when
            // the OS cannot supply that number; action validation still requires
            // a coherent live frame before any input is sent.
            if let explicitID { return explicitID == id }
            return cgBounds(entry).map { sameRect($0, frame) } ?? false
        }
        guard candidates.count == 1, let id = (candidates[0][kCGWindowNumber as String] as? NSNumber)?.uint32Value else { continue }
        let title = (try attribute(window, "AXTitle", optionalMetadata: true) as? String) ?? ""
        let minimized = (try attribute(window, "AXMinimized") as? Bool) == true
        result.append(AppWindow(binding: WindowBinding(element: window, id: id, owner: owner), frame: cgBounds(candidates[0]) ?? frame, title: String(title.prefix(400)), minimized: minimized))
    }
    return result
}
func selectWindow(_ identity: AppIdentity, requested: Any?) throws -> WindowBinding {
    let windows = try applicationWindows(identity)
    if let requested {
        let id: CGWindowID?
        if let number = requested as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID(), number.doubleValue > 0, number.doubleValue <= Double(UInt32.max), number.doubleValue.rounded() == number.doubleValue { id = number.uint32Value }
        else if let text = requested as? String { id = UInt32(text) }
        else { id = nil }
        guard let id, id > 0, let window = windows.first(where: { $0.binding.id == id }) else {
            throw ComputerError("WINDOW_NOT_FOUND", "Requested window does not belong to the selected application. List its windows and select an exact ID.")
        }
        return window.binding
    }
    if windows.count == 1 { return windows[0].binding }
    // Focus is used only to choose the initial window. The resulting ID and AX
    // object remain bound even if the user changes windows afterwards.
    if let focused = try elementAttribute(AXUIElementCreateApplication(identity.pid), "AXFocusedWindow"),
       let window = windows.first(where: { CFEqual($0.binding.element, focused) }) { return window.binding }
    throw ComputerError(windows.isEmpty ? "WINDOW_UNAVAILABLE" : "WINDOW_AMBIGUOUS", "No unique initial window. Use computer_windows and select_app with its window_id.")
}
func validateWindow(_ identity: AppIdentity, _ binding: WindowBinding, expectedSize: CGRect? = nil) throws -> CGRect {
    try requireAX(); try requireUnlocked(); try identity.running(); try binding.owner.validate()
    guard let owner = try? elementOwner(binding.element, identity), owner == binding.owner,
          let frame = try? bounds(binding.element), frame.width > 0, frame.height > 0 else {
        throw ComputerError("WINDOW_CHANGED", "Bound accessibility window was closed or its owner/geometry is unavailable. Select a window again.")
    }
    guard (try? attribute(binding.element, "AXMinimized") as? Bool) != true else { throw ComputerError("WINDOW_CHANGED", "Bound window is minimized. Restore it and select it again.") }
    guard let entry = windowInfo().first(where: { ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == binding.id }),
          (entry[kCGWindowOwnerPID as String] as? Int) == Int(binding.owner.pid),
          axWindowID(binding.element).map({ $0 == binding.id }) ?? true else {
        throw ComputerError("WINDOW_CHANGED", "Bound WindowServer identity was destroyed or replaced. Select a window again.")
    }
    guard let serverFrame = cgBounds(entry), sameRect(serverFrame, frame) else {
        throw ComputerError("STATE_CHANGED", "Window geometry is changing between accessibility and WindowServer. Observe again once the window settles.")
    }
    if let expectedSize, !sameSize(frame, expectedSize) {
        throw ComputerError("STATE_CHANGED", "Bound window was resized after observation. No input was dispatched. Observe again.")
    }
    return frame
}
func belongsToWindow(_ item: AXUIElement, _ window: AXUIElement) throws -> Bool {
    if CFEqual(item, window) { return true }
    if let ownerWindow = try elementAttribute(item, "AXWindow"), CFEqual(ownerWindow, window) { return true }
    var parent: AXUIElement? = item
    for _ in 0..<32 {
        guard let current = parent else { return false }
        if CFEqual(current, window) { return true }
        parent = try elementAttribute(current, "AXParent")
        if let parent, CFEqual(parent, current) { return false }
    }
    return false
}
func actionNames(_ item: AXUIElement) throws -> [String] {
    var names: CFArray?
    let status = AXUIElementCopyActionNames(item, &names)
    if [.attributeUnsupported, .noValue, .notImplemented].contains(status) { return [] }
    guard status == .success else { throw ComputerError("AX_UNAVAILABLE", "Cannot read accessibility actions (\(status.rawValue)).") }
    return names as? [String] ?? []
}
struct ElementState {
    let element: AXUIElement
    let owner: ProcessIdentity
    let role: String
    let subrole: String
    let identifier: String
    let title: String
    let enabled: Bool
    let box: CGRect?
    let actions: [String]
    let valueSettable: Bool
    let protected: Bool
    var secure: Bool { protected || subrole == "AXSecureTextField" || role == "AXSecureTextField" }
    func redacted() -> ElementState {
        ElementState(element: element, owner: owner, role: role, subrole: subrole, identifier: identifier, title: title, enabled: enabled, box: box, actions: [], valueSettable: false, protected: true)
    }
    static func read(_ item: AXUIElement, _ identity: AppIdentity, frame: CGRect) throws -> ElementState {
        let owner = try elementOwner(item, identity)
        let role = (try attribute(item, "AXRole") as? String) ?? "AXUnknown"
        let subrole = (try attribute(item, "AXSubrole") as? String) ?? ""
        let identifier = (try attribute(item, "AXIdentifier", optionalMetadata: true) as? String) ?? ""
        let title = (try attribute(item, "AXTitle", optionalMetadata: true) as? String) ?? ""
        let enabled = (try attribute(item, "AXEnabled") as? Bool) != false
        let protected = (try attribute(item, "AXProtectedContent") as? Bool) == true
        let box = try bounds(item).map { CGRect(x: $0.minX - frame.minX, y: $0.minY - frame.minY, width: $0.width, height: $0.height) }
        var settable: DarwinBoolean = false
        let settableStatus = AXUIElementIsAttributeSettable(item, "AXValue" as CFString, &settable)
        return ElementState(element: item, owner: owner, role: role, subrole: subrole, identifier: identifier, title: title, enabled: enabled, box: box, actions: try actionNames(item), valueSettable: settableStatus == .success && settable.boolValue, protected: protected)
    }
    func validate(_ identity: AppIdentity, _ binding: WindowBinding, frame: CGRect, geometry: Bool = true) throws -> ElementState {
        try owner.validate()
        guard try belongsToWindow(element, binding.element) else { throw ComputerError("STATE_CHANGED", "Referenced element is no longer inside the bound window. Observe again.") }
        let current = try ElementState.read(element, identity, frame: frame)
        guard current.owner == owner, current.role == role, current.subrole == subrole,
              current.identifier == identifier, current.title == title, current.enabled == enabled,
              !geometry || (box == nil && current.box == nil) || (box != nil && current.box != nil && sameRect(box!, current.box!)) else {
            throw ComputerError("STATE_CHANGED", "Referenced control identity, label, enabled state or position changed. No input was dispatched. Observe again.")
        }
        return current
    }
}
struct Observation {
    let binding: WindowBinding
    let frame: CGRect
    let focused: ElementState?
    let nodes: [[String: Any]]
    let refs: [String: ElementState]
    let truncated: Bool
    var window: AXUIElement { binding.element }
    var windowID: CGWindowID { binding.id }
    var json: [String: Any] { ["window": ["bounds": rectJSON(frame), "id": Int(windowID), "pid": binding.owner.pid], "elements": nodes, "truncated": truncated] }
    func withFrame(_ frame: CGRect) -> Observation { Observation(binding: binding, frame: frame, focused: focused, nodes: nodes, refs: refs, truncated: truncated) }
}
func focusedElement(_ identity: AppIdentity, _ binding: WindowBinding) throws -> AXUIElement? {
    // App-local focus can stay valid while another application is frontmost.
    // Never consult the system-wide AXFocusedApplication or follow a new window.
    let app = AXUIElementCreateApplication(binding.owner.pid)
    if let item = try elementAttribute(app, "AXFocusedUIElement"),
       (try? elementOwner(item, identity)) != nil, try belongsToWindow(item, binding.element) { return item }
    return nil
}
func observe(_ identity: AppIdentity, _ binding: WindowBinding) throws -> Observation {
    let frame = try validateWindow(identity, binding)
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
    try walk(binding.element, 0)
    if focused == nil, let candidateFocus { focused = try ElementState.read(candidateFocus, identity, frame: frame) }
    let finalFrame = try validateWindow(identity, binding, expectedSize: frame)
    guard sameRect(finalFrame, frame) else { throw ComputerError("STATE_CHANGED", "Bound window moved while its accessibility tree was being read. Observe again.") }
    return Observation(binding: binding, frame: finalFrame, focused: focused, nodes: nodes, refs: refs, truncated: truncated)
}
func screenshot(_ identity: AppIdentity, _ state: Observation) async throws -> [String: Any] {
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
    let data = NSMutableData()
    guard let destination = CGImageDestinationCreateWithData(data as CFMutableData, UTType.png.identifier as CFString, 1, nil) else { throw ComputerError("CAPTURE_FAILED", "PNG encoder unavailable.") }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination), data.length < 4 * 1024 * 1024 else { throw ComputerError("CAPTURE_FAILED", "Screenshot exceeds the image limit.") }
    return ["mimeType": "image/png", "data": (data as Data).base64EncodedString(), "width": image.width, "height": image.height]
}

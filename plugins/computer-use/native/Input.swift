import AppKit
import ApplicationServices
import Foundation
import Darwin

// macOS exposes PID delivery, but its public CGEvent constructors omit the
// destination window/local point. Resolve the same window-routing SPI used by
// desktop automation drivers, and fail closed when the OS does not provide it.
// There is deliberately no global event-tap fallback.
final class WindowPointerPoster {
    typealias Post = @convention(c) (pid_t, CGEvent) -> Void
    typealias SetLocation = @convention(c) (CGEvent, CGFloat, CGFloat) -> Void
    typealias SetField = @convention(c) (CGEvent, UInt32, Int64) -> Void
    let library: UnsafeMutableRawPointer
    let post: Post
    let setLocation: SetLocation
    let setField: SetField

    init() throws {
        guard let library = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY | RTLD_LOCAL) else {
            throw ComputerError("POINTER_ROUTING_UNAVAILABLE", "Window-scoped pointer delivery is unavailable on this macOS version. Use accessibility references.")
        }
        guard let post = dlsym(library, "SLEventPostToPid"), let location = dlsym(library, "CGEventSetWindowLocation"), let field = dlsym(library, "SLEventSetIntegerValueField") else {
            dlclose(library)
            throw ComputerError("POINTER_ROUTING_UNAVAILABLE", "Window-scoped pointer delivery is unavailable on this macOS version. Use accessibility references.")
        }
        self.library = library
        self.post = unsafeBitCast(post, to: Post.self)
        self.setLocation = unsafeBitCast(location, to: SetLocation.self)
        self.setField = unsafeBitCast(field, to: SetField.self)
    }
    deinit { dlclose(library) }
    func stamp(_ event: CGEvent, _ state: Observation, _ identity: AppIdentity, _ point: CGPoint) throws {
        guard let windowID = state.windowID else { throw ComputerError("WINDOW_AMBIGUOUS", "Pointer input requires an identified window.") }
        setLocation(event, point.x - state.frame.minX, point.y - state.frame.minY)
        setField(event, 40, Int64(identity.pid))
        for field: UInt32 in [51, 91, 92] { setField(event, field, Int64(windowID)) }
        event.timestamp = DispatchTime.now().uptimeNanoseconds
        event.flags = []
    }
}

func requireForeground(_ identity: AppIdentity) throws {
    _ = try identity.running()
    // Query WindowServer's accessibility focus directly. NSWorkspace caches its
    // foreground property until the main run loop advances, including while a
    // long AX tree is being read, so it cannot be this input boundary's authority.
    let system = AXUIElementCreateSystemWide()
    let foreground = try elementAttribute(system, "AXFocusedApplication")
    var foregroundPID: pid_t = 0
    guard let foreground, AXUIElementGetPid(foreground, &foregroundPID) == .success, foregroundPID == identity.pid else {
        throw ComputerError("FOCUS_CHANGED", "Selected app is no longer frontmost. No input was dispatched. Select it explicitly and observe again.")
    }
}
func targetPoint(_ params: [String: Any], _ state: Observation, _ identity: AppIdentity) throws -> CGPoint {
    guard let x = params["x"] as? Double, let y = params["y"] as? Double, x.isFinite, y.isFinite,
          x >= 0, y >= 0, x < state.frame.width, y < state.frame.height else {
        throw ComputerError("INVALID_COORDINATES", "Coordinates must be inside the observed window, in window points.")
    }
    let point = CGPoint(x: state.frame.minX + x, y: state.frame.minY + y)
    var hit: AXUIElement?
    let status = AXUIElementCopyElementAtPosition(AXUIElementCreateSystemWide(), Float(point.x), Float(point.y), &hit)
    var owner: pid_t = 0
    guard status == .success, let hit, AXUIElementGetPid(hit, &owner) == .success, owner == identity.pid else {
        throw ComputerError("TARGET_OBSCURED", "Point is not exposed in the selected application. No input was dispatched.")
    }
    return point
}
func postKey(_ code: CGKeyCode, _ identity: AppIdentity, text: [UniChar]? = nil) throws {
    try requireForeground(identity)
    guard let down = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: true),
          let up = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false) else { throw ComputerError("INPUT_UNAVAILABLE", "Cannot create keyboard event.") }
    if let text {
        text.withUnsafeBufferPointer { pointer in
            if let base = pointer.baseAddress {
                down.keyboardSetUnicodeString(stringLength: text.count, unicodeString: base)
                up.keyboardSetUnicodeString(stringLength: text.count, unicodeString: base)
            }
        }
    }
    down.postToPid(identity.pid); up.postToPid(identity.pid)
}
func dispatchInput(_ method: String, _ params: [String: Any], _ state: Observation, _ identity: AppIdentity) throws {
    try requireForeground(identity)
    switch method {
    case "click":
        if let ref = params["ref"] as? String {
            guard let element = state.refs[ref] else { throw ComputerError("INVALID_REF", "Element ref is not in the current observation.") }
            var owner: pid_t = 0
            guard AXUIElementGetPid(element, &owner) == .success, owner == identity.pid else {
                throw ComputerError("TARGET_OUTSIDE_APP", "Accessibility target belongs to another process. No input was dispatched.")
            }
            let status = AXUIElementPerformAction(element, kAXPressAction as CFString)
            guard status == .success else { throw ComputerError("ACTION_FAILED", "AX press was not accepted (\(status.rawValue)). Observe before choosing another action.") }
        } else {
            let point = try targetPoint(params, state, identity)
            let poster = try WindowPointerPoster()
            let source = CGEventSource(stateID: .hidSystemState)
            guard let down = CGEvent(mouseEventSource: source, mouseType: .leftMouseDown, mouseCursorPosition: point, mouseButton: .left),
                  let up = CGEvent(mouseEventSource: source, mouseType: .leftMouseUp, mouseCursorPosition: point, mouseButton: .left) else { throw ComputerError("INPUT_UNAVAILABLE", "Cannot create mouse event.") }
            let group = Int64(bitPattern: DispatchTime.now().uptimeNanoseconds)
            for event in [down, up] {
                try poster.stamp(event, state, identity, point)
                poster.setField(event, 1, 1) // click count
                poster.setField(event, 3, 0) // left button
                poster.setField(event, 7, 3) // window-routed mouse subtype
                poster.setField(event, 58, group)
            }
            try requireForeground(identity)
            poster.post(identity.pid, down); poster.post(identity.pid, up)
        }
    case "type":
        guard let text = params["text"] as? String, !text.isEmpty, text.utf16.count <= 2000 else { throw ComputerError("INVALID_TEXT", "Text must contain 1–2000 UTF-16 units.") }
        guard let focused = state.focused, (try attribute(focused, "AXSubrole") as? String) != "AXSecureTextField" else { throw ComputerError("FOCUS_UNAVAILABLE", "No supported focused control. Secret/password fields require user input.") }
        // Short Unicode events avoid the pasteboard and preserve surrogate pairs.
        var chunk: [UniChar] = []
        for scalar in text.unicodeScalars {
            let units = Array(String(scalar).utf16)
            if chunk.count + units.count > 20 { try postKey(0, identity, text: chunk); chunk = [] }
            chunk.append(contentsOf: units)
        }
        if !chunk.isEmpty { try postKey(0, identity, text: chunk) }
    case "key":
        let codes: [String: CGKeyCode] = ["enter": 36, "tab": 48, "escape": 53, "backspace": 51, "delete": 117, "left": 123, "right": 124, "up": 126, "down": 125, "home": 115, "end": 119, "pageup": 116, "pagedown": 121]
        guard let name = params["key"] as? String, let code = codes[name] else { throw ComputerError("INVALID_KEY", "Unsupported navigation key.") }
        try postKey(code, identity)
    case "scroll":
        let point = try targetPoint(params, state, identity)
        guard let delta = params["delta_y"] as? Int32, delta >= -1000, delta <= 1000,
              let event = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: delta, wheel2: 0, wheel3: 0) else { throw ComputerError("INVALID_SCROLL", "Scroll distance must be an integer between -1000 and 1000.") }
        event.location = point
        let poster = try WindowPointerPoster()
        try poster.stamp(event, state, identity, point)
        try requireForeground(identity)
        poster.post(identity.pid, event)
    default: throw ComputerError("UNKNOWN_METHOD", "Unknown input operation.")
    }
}

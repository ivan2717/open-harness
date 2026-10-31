import AppKit
import ApplicationServices
import Foundation
import Darwin
import Carbon

// EOF and SIGTERM stop new input while giving an in-flight mouse transaction
// time to send its matching release. SIGKILL/process crashes cannot be cleaned up.
enum InputCancellation {
    private static let lock = NSLock()
    private static var stopped = false
    private static var signalSource: DispatchSourceSignal?
    static func install() {
        signal(SIGTERM, SIG_IGN)
        signal(SIGPIPE, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .global(qos: .userInitiated))
        source.setEventHandler { cancel() }
        source.resume()
        signalSource = source
    }
    static func cancel() {
        lock.lock(); let first = !stopped; stopped = true; lock.unlock()
        if first { DispatchQueue.global().asyncAfter(deadline: .now() + 0.45) { exit(0) } }
    }
    static func check() throws {
        lock.lock(); let cancelled = stopped; lock.unlock()
        if cancelled || Task.isCancelled { throw ComputerError("INPUT_CANCELLED", "The input channel was cancelled. No new input was dispatched.") }
    }
}

// WindowServer PID delivery plus explicit destination-window/local-point fields.
// No global event-tap posting, cursor warp, app activation or system AX hit-test.
// This routing SPI is version-dependent: missing symbols fail closed.
final class WindowEventPoster {
    typealias Post = @convention(c) (pid_t, CGEvent) -> Void
    typealias SetLocation = @convention(c) (CGEvent, CGFloat, CGFloat) -> Void
    typealias SetField = @convention(c) (CGEvent, UInt32, Int64) -> Void
    let library: UnsafeMutableRawPointer
    let post: Post
    let setLocation: SetLocation
    let setField: SetField
    init() throws {
        guard let library = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY | RTLD_LOCAL) else {
            throw ComputerError("INPUT_ROUTING_UNAVAILABLE", "Window-scoped input delivery is unavailable on this macOS version. Use accessibility actions.")
        }
        guard let post = dlsym(library, "SLEventPostToPid"), let location = dlsym(library, "CGEventSetWindowLocation"), let field = dlsym(library, "SLEventSetIntegerValueField") else {
            dlclose(library)
            throw ComputerError("INPUT_ROUTING_UNAVAILABLE", "Window-scoped input delivery is unavailable on this macOS version. Use accessibility actions.")
        }
        self.library = library
        self.post = unsafeBitCast(post, to: Post.self)
        self.setLocation = unsafeBitCast(location, to: SetLocation.self)
        self.setField = unsafeBitCast(field, to: SetField.self)
    }
    deinit { dlclose(library) }
    func stamp(_ event: CGEvent, _ state: Observation, local: CGPoint?, flags: CGEventFlags = []) {
        if let local {
            event.location = CGPoint(x: state.frame.minX + local.x, y: state.frame.minY + local.y)
            setLocation(event, local.x, local.y)
        }
        setField(event, 40, Int64(state.binding.owner.pid))
        for field: UInt32 in [51, 91, 92] { setField(event, field, Int64(state.windowID)) }
        event.timestamp = DispatchTime.now().uptimeNanoseconds
        event.flags = flags
    }
}
// A held button always has a preallocated matching release. Cleanup addresses
// only the original process birth identity/window ID, even if normal validation
// fails after down. It never releases a physical/global desktop mouse button.
final class HeldPointer {
    let poster: WindowEventPoster
    let identity: AppIdentity
    let releaseEvent: CGEvent
    let button: CGMouseButton
    let count: Int64
    let group: Int64
    let flags: CGEventFlags
    var state: Observation
    var local: CGPoint
    private var held = false
    init(poster: WindowEventPoster, identity: AppIdentity, state: Observation, local: CGPoint, releaseEvent: CGEvent, button: CGMouseButton, count: Int64, group: Int64, flags: CGEventFlags) {
        self.poster = poster; self.identity = identity; self.state = state; self.local = local; self.releaseEvent = releaseEvent; self.button = button; self.count = count; self.group = group; self.flags = flags
    }
    func stamp(_ event: CGEvent) {
        poster.stamp(event, state, local: local, flags: flags)
        poster.setField(event, 1, count); poster.setField(event, 3, Int64(button.rawValue)); poster.setField(event, 7, 3); poster.setField(event, 58, group)
    }
    func begin(_ event: CGEvent) {
        stamp(event); held = true
        poster.post(state.binding.owner.pid, event)
    }
    @discardableResult func release() -> Bool {
        guard held else { return true }
        held = false
        // A cleanup release is allowed after cancellation/lock/resize, but never
        // after PID reuse or delivery to another window/process. No AX round-trip
        // is needed in this bounded shutdown path.
        guard (try? identity.process.validate()) != nil, (try? state.binding.owner.validate()) != nil,
              let entry = windowInfo().first(where: { ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == state.windowID }),
              (entry[kCGWindowOwnerPID as String] as? Int) == Int(state.binding.owner.pid),
              let frame = cgBounds(entry), frame.width > 0, frame.height > 0 else { return false }
        state = state.withFrame(frame)
        local = CGPoint(x: min(max(0, local.x), frame.width - 1), y: min(max(0, local.y), frame.height - 1))
        stamp(releaseEvent)
        poster.post(state.binding.owner.pid, releaseEvent)
        return true
    }
    deinit { release() }
}

// Prefer the smallest observed interactive control under a local point. Only
// that control is revalidated: animated labels elsewhere, and canvases with no
// meaningful AX controls, do not trigger a full-tree or pixel equality check.
func coordinateTarget(_ local: CGPoint, _ observation: Observation) -> ElementState? {
    let interactiveRoles: Set<String> = ["AXButton", "AXCheckBox", "AXRadioButton", "AXPopUpButton", "AXMenuButton", "AXMenuItem", "AXTextField", "AXSecureTextField", "AXTextArea", "AXComboBox", "AXSlider", "AXScrollBar", "AXScrollArea", "AXLink", "AXDisclosureTriangle", "AXIncrementor", "AXRow", "AXCell"]
    return observation.refs.values.filter { item in
        guard let box = item.box, box.width > 0, box.height > 0, box.contains(local) else { return false }
        return !item.actions.isEmpty || item.valueSettable || item.secure || interactiveRoles.contains(item.role)
    }.min { a, b in
        let aBox = a.box!; let bBox = b.box!
        return aBox.width * aBox.height < bBox.width * bBox.height
    }
}
func validateCoordinateTarget(_ local: CGPoint, _ observation: Observation, _ identity: AppIdentity, frame: CGRect) throws {
    guard let saved = coordinateTarget(local, observation) else { return }
    do {
        let current = try saved.validate(identity, observation.binding, frame: frame)
        guard current.enabled else { throw ComputerError("TARGET_DISABLED", "The control under this coordinate is disabled.") }
    } catch let error as ComputerError {
        throw ComputerError("STATE_CHANGED", "The observed control under this coordinate changed: \(error.message) Observe again.")
    }
}

func modifierFlags(_ raw: Any?) throws -> CGEventFlags {
    guard let raw else { return [] }
    guard let modifiers = raw as? [String], modifiers.count <= 4, Set(modifiers).count == modifiers.count else {
        throw ComputerError("INVALID_MODIFIERS", "Modifiers must be distinct shift, control, alt or meta strings.")
    }
    let values: [String: CGEventFlags] = ["shift": .maskShift, "control": .maskControl, "alt": .maskAlternate, "meta": .maskCommand]
    var flags: CGEventFlags = []
    for name in modifiers {
        guard let flag = values[name] else { throw ComputerError("INVALID_MODIFIERS", "Unsupported modifier: \(name).") }
        flags.insert(flag)
    }
    return flags
}
func targetPoint(_ params: [String: Any], _ state: Observation, xKey: String = "x", yKey: String = "y") throws -> CGPoint {
    guard let x = params[xKey] as? Double, let y = params[yKey] as? Double, x.isFinite, y.isFinite,
          x >= 0, y >= 0, x < state.frame.width, y < state.frame.height else {
        throw ComputerError("INVALID_COORDINATES", "Coordinates must be inside the observed window, in window-local points.")
    }
    return CGPoint(x: x, y: y)
}
func requireNonSecure(_ target: ElementState, _ window: AXUIElement?) throws {
    guard let window else { throw ComputerError("TARGET_UNAVAILABLE", "Accessibility window is unavailable for this semantic action. Observe again.") }
    var item: AXUIElement? = target.element
    for _ in 0..<32 {
        guard let current = item else { return }
        let subrole = try attribute(current, "AXSubrole") as? String
        let role = try attribute(current, "AXRole") as? String
        let protected = try attribute(current, "AXProtectedContent") as? Bool
        guard subrole != "AXSecureTextField", role != "AXSecureTextField", protected != true else {
            throw ComputerError("SECURE_INPUT_UNSUPPORTED", "Password and protected controls require user input. No input was dispatched.")
        }
        if CFEqual(current, window) { return }
        item = try elementAttribute(current, "AXParent")
    }
    throw ComputerError("TARGET_UNAVAILABLE", "Cannot verify whether this control contains protected input.")
}
func checkedFocus(_ previous: Observation, _ identity: AppIdentity, frame: CGRect) throws -> ElementState? {
    try validateKeyboardWindow(identity, previous.binding, frame: frame)
    guard !IsSecureEventInputEnabled() else { throw ComputerError("SECURE_INPUT_UNSUPPORTED", "Secure keyboard input is active. The user must complete protected input directly.") }
    guard let prior = previous.focused else {
        // Screenshot-based canvas input does not require an AX responder.
        // Still reject a newly exposed control and any known protected focus.
        if let current = try? elementAttribute(AXUIElementCreateApplication(previous.binding.owner.pid), "AXFocusedUIElement") {
            let role = try attribute(current, "AXRole") as? String
            let subrole = try attribute(current, "AXSubrole") as? String
            guard role != "AXSecureTextField", subrole != "AXSecureTextField", (try attribute(current, "AXProtectedContent") as? Bool) != true else {
                throw ComputerError("SECURE_INPUT_UNSUPPORTED", "The keyboard target is protected. The user must enter it directly.")
            }
            if let window = previous.window, (try? belongsToWindow(current, window)) == true {
                throw ComputerError("FOCUS_CHANGED", "A focused control appeared after observation. Observe again before typing.")
            }
        }
        return nil
    }
    guard let focused = try focusedElement(identity, previous.binding), CFEqual(prior.element, focused) else {
        throw ComputerError("FOCUS_CHANGED", "The bound window's focused control changed or is unavailable. No input was dispatched. Observe again.")
    }
    guard !prior.secure else { throw ComputerError("SECURE_INPUT_UNSUPPORTED", "The observed focus was a password or protected control. No input was dispatched.") }
    let current = try prior.validate(identity, previous.binding, frame: frame, geometry: false)
    try requireNonSecure(current, previous.window)
    guard current.enabled else { throw ComputerError("TARGET_DISABLED", "Focused control is disabled.") }
    return current
}
func parseKey(_ raw: Any?) throws -> (CGKeyCode, CGEventFlags) {
    let codes: [String: CGKeyCode] = [
        "a":0,"s":1,"d":2,"f":3,"h":4,"g":5,"z":6,"x":7,"c":8,"v":9,"b":11,"q":12,"w":13,"e":14,"r":15,"y":16,"t":17,
        "1":18,"2":19,"3":20,"4":21,"6":22,"5":23,"9":25,"7":26,"8":28,"0":29,"o":31,"u":32,"i":34,"p":35,"l":37,"j":38,"k":40,"n":45,"m":46,
        "enter":36,"tab":48,"space":49,"escape":53,"backspace":51,"delete":117,"left":123,"right":124,"up":126,"down":125,"home":115,"end":119,"pageup":116,"pagedown":121,
        "f1":122,"f2":120,"f3":99,"f4":118,"f5":96,"f6":97,"f7":98,"f8":100,"f9":101,"f10":109,"f11":103,"f12":111]
    guard let text = raw as? String, !text.isEmpty, text.count < 80 else { throw ComputerError("INVALID_KEY", "Specify one key, optionally preceded by modifiers joined with +.") }
    let parts = text.lowercased().split(separator: "+", omittingEmptySubsequences: false).map(String.init)
    let aliases = ["cmd":"meta", "command":"meta", "ctrl":"control", "option":"alt"]
    let modifiers = parts.dropLast().map { aliases[$0] ?? $0 }
    guard let name = parts.last, let code = codes[name] else { throw ComputerError("INVALID_KEY", "Unsupported key. Use a letter, digit, F1–F12 or navigation key.") }
    let flags = try modifierFlags(modifiers)
    // App switching, launcher and screen-lock shortcuts have system semantics.
    // They are never part of a window-scoped input request.
    guard !(name == "tab" && (flags.contains(.maskCommand) || flags.contains(.maskAlternate))),
          !(name == "space" && flags.contains(.maskCommand)),
          !(name == "q" && flags.contains(.maskCommand) && flags.contains(.maskControl)),
          !(name == "delete" && flags.contains(.maskAlternate) && flags.contains(.maskControl)) else {
        throw ComputerError("UNSUPPORTED_SYSTEM_SHORTCUT", "This shortcut changes system focus or session state and is not supported by window-scoped input.")
    }
    return (code, flags)
}
func requiresScreenshot(_ method: String, _ params: [String: Any]) -> Bool {
    if method == "scroll" || method == "drag" { return true }
    if method != "click" { return false }
    return params["ref"] == nil || (params["button"] as? String ?? "left") != "left" || (params["click_count"] as? Int ?? 1) != 1 || !(params["modifiers"] as? [String] ?? []).isEmpty
}
@MainActor func dispatchInput(_ method: String, _ params: [String: Any], _ previous: Observation, _ identity: AppIdentity, screenshotImage: CGImage?) async throws -> [String: Any] {
    var dispatched = false
    var outcomeVerified = false
    var transport = "window_events"
    var heldPointer: HeldPointer?
    defer { heldPointer?.release() }
    do {
        try InputCancellation.check()
        var state = previous.withFrame(try validateWindow(identity, previous.binding, expectedSize: previous.frame))
        let target: ElementState?
        if let ref = params["ref"] as? String {
            guard let saved = previous.refs[ref] else { throw ComputerError("INVALID_REF", "Element ref is not in the consumed observation.") }
            guard !saved.secure else { throw ComputerError("SECURE_INPUT_UNSUPPORTED", "The observed control was a password or protected control. No input was dispatched.") }
            target = try saved.validate(identity, state.binding, frame: state.frame)
            guard target!.enabled else { throw ComputerError("TARGET_DISABLED", "Referenced control is disabled.") }
        } else { target = nil }
        func refreshWindow() throws {
            try InputCancellation.check()
            state = state.withFrame(try validateWindow(identity, state.binding, expectedSize: previous.frame))
        }
        func perform(_ element: ElementState, action: String) throws {
            guard element.actions.contains(action) else { throw ComputerError("ACTION_UNAVAILABLE", "This control no longer exposes the requested accessibility action. Observe again.") }
            try refreshWindow()
            try requireNonSecure(element, state.window)
            let status = AXUIElementPerformAction(element.element, action as CFString)
            // An AX timeout may still mean the application accepted the action.
            guard status == .success else { throw ComputerError("ACTION_FAILED", "Accessibility action did not report success (\(status.rawValue)). Observe before retrying.", dispatched: nil) }
            dispatched = true; transport = "accessibility_action"
        }
        switch method {
        case "click":
            let buttonName = params["button"] as? String ?? "left"
            let count = params["click_count"] as? Int ?? 1
            let flags = try modifierFlags(params["modifiers"])
            guard ["left", "right", "middle"].contains(buttonName), (1...3).contains(count) else { throw ComputerError("INVALID_CLICK", "Button must be left, right or middle; click_count must be 1–3.") }
            if let target, buttonName == "left", count == 1, flags.isEmpty {
                guard let ref = params["ref"] as? String, previous.refs[ref]?.actions.contains(kAXPressAction as String) == true else {
                    throw ComputerError("ACTION_UNAVAILABLE", "The observed control did not expose AXPress. Use an advertised action or a screenshot-based coordinate click.")
                }
                try perform(target, action: kAXPressAction as String)
                break
            }
            let local: CGPoint
            if let target {
                guard let box = target.box, box.width > 0, box.height > 0 else { throw ComputerError("TARGET_UNAVAILABLE", "Referenced control has no usable bounds for pointer input.") }
                local = try targetPoint(["x": box.midX, "y": box.midY], state)
            } else { local = try targetPoint(params, state) }
            try await validateVisualTargets([local], previous, identity, screenshotImage)
            let poster = try WindowEventPoster()
            let source = CGEventSource(stateID: .privateState)
            let button: CGMouseButton = buttonName == "right" ? .right : buttonName == "middle" ? .center : .left
            let downType: CGEventType = button == .right ? .rightMouseDown : button == .center ? .otherMouseDown : .leftMouseDown
            let upType: CGEventType = button == .right ? .rightMouseUp : button == .center ? .otherMouseUp : .leftMouseUp
            let group = Int64(bitPattern: DispatchTime.now().uptimeNanoseconds)
            for click in 1...count {
                try refreshWindow()
                guard let down = CGEvent(mouseEventSource: source, mouseType: downType, mouseCursorPosition: .zero, mouseButton: button),
                      let up = CGEvent(mouseEventSource: source, mouseType: upType, mouseCursorPosition: .zero, mouseButton: button) else { throw ComputerError("INPUT_UNAVAILABLE", "Cannot create mouse events.") }
                try validateCoordinateTarget(local, previous, identity, frame: state.frame)
                let transaction = HeldPointer(poster: poster, identity: identity, state: state, local: local, releaseEvent: up, button: button, count: Int64(click), group: group, flags: flags)
                heldPointer = transaction
                transaction.begin(down); dispatched = true
                guard transaction.release() else { throw ComputerError("INPUT_UNCERTAIN", "The target disappeared after mouse-down; release could not be delivered to the original window.", dispatched: nil) }
            }
        case "type", "key":
            let focus = try checkedFocus(previous, identity, frame: state.frame)
            // AppKit dispatches Command+A through its application menu when the
            // app is active. For an observed editable text control, use the
            // standard AX selected-text range directly; this has the same text
            // selection semantics without changing app/key-window activation.
            // Choose this transport before dispatch, never after an uncertain key.
            if method == "key" {
                let (code, flags) = try parseKey(params["key"])
                if let focus, code == 0, flags == .maskCommand,
                   ["AXTextField", "AXTextArea", "AXComboBox"].contains(focus.role) {
                    var settable: DarwinBoolean = false
                    guard AXUIElementIsAttributeSettable(focus.element, "AXSelectedTextRange" as CFString, &settable) == .success, settable.boolValue else {
                        throw ComputerError("ACTION_UNSUPPORTED", "This background text control does not expose a writable selection range for Command+A.")
                    }
                    try refreshWindow()
                    _ = try checkedFocus(previous, identity, frame: state.frame)
                    let number = try attribute(focus.element, "AXNumberOfCharacters") as? NSNumber
                    let value = try attribute(focus.element, "AXValue", optionalMetadata: true) as? String
                    guard let length = number.map({ $0.intValue }) ?? value.map({ ($0 as NSString).length }), length >= 0 else {
                        throw ComputerError("ACTION_UNSUPPORTED", "Cannot determine this text control's selection range.")
                    }
                    var range = CFRange(location: 0, length: length)
                    guard let selectedRange = AXValueCreate(.cfRange, &range) else { throw ComputerError("INPUT_UNAVAILABLE", "Cannot create the accessibility selection range.") }
                    let status = AXUIElementSetAttributeValue(focus.element, "AXSelectedTextRange" as CFString, selectedRange)
                    guard status == .success else { throw ComputerError("ACTION_FAILED", "Setting the text selection did not report success (\(status.rawValue)). Observe before retrying.", dispatched: nil) }
                    dispatched = true; transport = "accessibility_selection"
                    if let readback = try? attribute(focus.element, "AXSelectedTextRange"), CFGetTypeID(readback) == AXValueGetTypeID() {
                        var actual = CFRange()
                        outcomeVerified = AXValueGetValue(unsafeDowncast(readback, to: AXValue.self), .cfRange, &actual) && actual.location == 0 && actual.length == length
                    }
                    break
                }
            }
            let poster = try WindowEventPoster()
            let source = CGEventSource(stateID: .privateState)
            func postKey(_ code: CGKeyCode, flags: CGEventFlags = [], text: [UniChar]? = nil) throws {
                try refreshWindow()
                _ = try checkedFocus(previous, identity, frame: state.frame)
                guard let down = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: true),
                      let up = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: false) else { throw ComputerError("INPUT_UNAVAILABLE", "Cannot create keyboard events.") }
                for event in [down, up] {
                    if let text { text.withUnsafeBufferPointer { pointer in if let base = pointer.baseAddress { event.keyboardSetUnicodeString(stringLength: text.count, unicodeString: base) } } }
                    poster.stamp(event, state, local: nil, flags: flags)
                    // AX may expose a remote editor from a verified helper,
                    // but WindowServer events belong to the process that owns
                    // this window. The owner routes them to its own renderer.
                    poster.post(state.binding.owner.pid, event); dispatched = true
                }
            }
            if method == "type" {
                guard let text = params["text"] as? String, !text.isEmpty, text.utf16.count <= 2000 else { throw ComputerError("INVALID_TEXT", "Text must contain 1–2000 UTF-16 units.") }
                var chunk: [UniChar] = []
                for scalar in text.unicodeScalars {
                    let units = Array(String(scalar).utf16)
                    if chunk.count + units.count > 20 { try postKey(0, text: chunk); chunk = [] }
                    chunk.append(contentsOf: units)
                }
                if !chunk.isEmpty { try postKey(0, text: chunk) }
            } else { let (code, flags) = try parseKey(params["key"]); try postKey(code, flags: flags) }
        case "scroll":
            let local = try targetPoint(params, state)
            guard let delta = params["delta_y"] as? Int32, delta >= -1000, delta <= 1000,
                  let event = CGEvent(scrollWheelEvent2Source: CGEventSource(stateID: .privateState), units: .pixel, wheelCount: 1, wheel1: delta, wheel2: 0, wheel3: 0) else { throw ComputerError("INVALID_SCROLL", "Scroll distance must be an integer between -1000 and 1000.") }
            let poster = try WindowEventPoster()
            try await validateVisualTargets([local], previous, identity, screenshotImage)
            try refreshWindow()
            try validateCoordinateTarget(local, previous, identity, frame: state.frame)
            poster.stamp(event, state, local: local)
            poster.post(state.binding.owner.pid, event); dispatched = true
        case "drag":
            let from = try targetPoint(params, state, xKey: "from_x", yKey: "from_y")
            let to = try targetPoint(params, state, xKey: "to_x", yKey: "to_y")
            let flags = try modifierFlags(params["modifiers"])
            try await validateVisualTargets([from, to], previous, identity, screenshotImage)
            let poster = try WindowEventPoster()
            let source = CGEventSource(stateID: .privateState)
            let group = Int64(bitPattern: DispatchTime.now().uptimeNanoseconds)
            // A finite sequence stays in the bound window. Build all events
            // before sending down so allocation failure cannot leave a drag open.
            var events: [(CGEvent, CGPoint)] = []
            for step in 0...13 {
                let fraction = min(1, Double(step) / 12)
                let point = CGPoint(x: from.x + (to.x - from.x) * fraction, y: from.y + (to.y - from.y) * fraction)
                let type: CGEventType = step == 0 ? .leftMouseDown : step == 13 ? .leftMouseUp : .leftMouseDragged
                guard let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: .zero, mouseButton: .left) else { throw ComputerError("INPUT_UNAVAILABLE", "Cannot create drag events.") }
                events.append((event, point))
            }
            try refreshWindow()
            try validateCoordinateTarget(from, previous, identity, frame: state.frame)
            try validateCoordinateTarget(to, previous, identity, frame: state.frame)
            let transaction = HeldPointer(poster: poster, identity: identity, state: state, local: from, releaseEvent: events.last!.0, button: .left, count: 1, group: group, flags: flags)
            heldPointer = transaction
            transaction.begin(events[0].0); dispatched = true
            for (event, point) in events.dropFirst().dropLast() {
                // A short bounded cadence lets real apps process dragging and
                // makes channel cancellation observable between held events.
                usleep(8_000)
                try refreshWindow()
                transaction.state = state; transaction.local = point
                transaction.stamp(event)
                poster.post(state.binding.owner.pid, event)
            }
            transaction.local = to
            guard transaction.release() else { throw ComputerError("INPUT_UNCERTAIN", "The target disappeared during drag; release could not be delivered to the original window.", dispatched: nil) }
        case "set_value":
            guard let target else { throw ComputerError("INVALID_REF", "A current observed control ref is required.") }
            try requireNonSecure(target, state.window)
            guard let ref = params["ref"] as? String, let old = previous.refs[ref], old.valueSettable, target.valueSettable,
                  let value = params["value"] as? String, value.utf16.count <= 2000 else { throw ComputerError("VALUE_UNAVAILABLE", "A writable observed control and a string up to 2000 UTF-16 units are required.") }
            try refreshWindow()
            let status = AXUIElementSetAttributeValue(target.element, "AXValue" as CFString, value as CFString)
            guard status == .success else { throw ComputerError("ACTION_FAILED", "Setting the accessibility value did not report success (\(status.rawValue)). Observe before retrying.", dispatched: nil) }
            dispatched = true; transport = "accessibility_value"
            outcomeVerified = (try? attribute(target.element, "AXValue") as? String) == value
        case "action":
            guard let target, let ref = params["ref"] as? String, let action = params["action"] as? String,
                  previous.refs[ref]?.actions.contains(action) == true else { throw ComputerError("ACTION_UNAVAILABLE", "Only actions actually listed for this ref in the consumed observation are allowed.") }
            try requireNonSecure(target, state.window)
            try perform(target, action: action)
        default: throw ComputerError("UNKNOWN_METHOD", "Unknown input operation.")
        }
    } catch let error as ComputerError {
        if dispatched { throw ComputerError("INPUT_UNCERTAIN", "Some input was dispatched before validation failed: \(error.message) Observe and verify before continuing.", dispatched: nil) }
        throw error
    }
    return ["dispatched": dispatched, "verified": outcomeVerified, "verification": ["status": outcomeVerified ? "accessibility_readback_matched" : "not_verified", "transport": transport], "next": "Observe again and verify the intended result before continuing."]
}

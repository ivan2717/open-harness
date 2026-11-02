import AppKit
import ApplicationServices
import Darwin
import Foundation

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

// The WindowServer ID and the owner's OS birth identity are authoritative.
// AX enriches a binding; failure to publish AXWindows must not hide a canvas.
struct WindowBinding {
    let element: AXUIElement?
    let id: CGWindowID
    let owner: ProcessIdentity
    func withElement(_ element: AXUIElement?) -> WindowBinding { WindowBinding(element: element, id: id, owner: owner) }
}
struct AppWindow {
    let binding: WindowBinding
    let frame: CGRect
    let title: String
    let onScreen: Bool
    var json: [String: Any] {
        var result: [String: Any] = ["id": Int(binding.id), "pid": binding.owner.pid, "title": title,
            "bounds": rectJSON(frame), "on_screen": onScreen, "accessibility": binding.element != nil]
        // Off-screen also means another Space or a hidden app, not just minimized.
        if let element = binding.element, let minimized = try? attribute(element, "AXMinimized") as? Bool { result["minimized"] = minimized }
        return result
    }
}

func axWindows(_ pid: pid_t) -> [AXUIElement] {
    let app = AXUIElementCreateApplication(pid)
    var result = (try? attribute(app, "AXWindows") as? [AXUIElement]) ?? []
    // Some hosted apps expose focused/main windows while AXWindows is empty.
    for name in ["AXFocusedWindow", "AXMainWindow"] {
        if let window = try? elementAttribute(app, name), !result.contains(where: { CFEqual($0, window) }) { result.append(window) }
    }
    return result
}
func matchesAXWindow(_ element: AXUIElement, _ binding: WindowBinding, frame: CGRect, candidates: [[String: Any]]) -> Bool {
    var pid: pid_t = 0
    guard AXUIElementGetPid(element, &pid) == .success, pid == binding.owner.pid else { return false }
    if let id = axWindowID(element) { return id == binding.id }
    guard let box = try? bounds(element), sameRect(box, frame) else { return false }
    // A geometry-only match is usable only when unique within the owner.
    return candidates.filter { ($0[kCGWindowOwnerPID as String] as? Int) == Int(pid) && cgBounds($0).map { sameRect($0, box) } == true }.count == 1
}
func accessibleWindow(_ identity: AppIdentity, _ binding: WindowBinding, frame: CGRect) -> AXUIElement? {
    let entries = windowInfo()
    var candidates: [AXUIElement] = binding.element.map { [$0] } ?? []
    candidates += axWindows(binding.owner.pid)
    if binding.owner.pid != identity.pid { candidates += axWindows(identity.pid) }
    return candidates.first { matchesAXWindow($0, binding, frame: frame, candidates: entries) && (try? bounds($0)).map { sameRect($0, frame) } == true }
}
func applicationWindows(_ identity: AppIdentity) throws -> [AppWindow] {
    try requireAX(); try identity.running()
    let entries = windowInfo()
    var owners: [pid_t: ProcessIdentity] = [:]
    var rejected: Set<pid_t> = []
    var candidates = axWindows(identity.pid)
    var result: [AppWindow] = []
    for entry in entries {
        guard (entry[kCGWindowLayer as String] as? Int) == 0,
              let rawPID = entry[kCGWindowOwnerPID as String] as? Int32, rawPID > 0,
              let id = (entry[kCGWindowNumber as String] as? NSNumber)?.uint32Value, id > 0,
              let frame = cgBounds(entry), frame.width > 1, frame.height > 1,
              frame.minX.isFinite, frame.minY.isFinite, frame.width.isFinite, frame.height.isFinite else { continue }
        if rejected.contains(rawPID) { continue }
        if owners[rawPID] == nil {
            guard let owner = try? identity.trustedProcess(rawPID) else { rejected.insert(rawPID); continue }
            owners[rawPID] = owner
            if rawPID != identity.pid { candidates += axWindows(rawPID) }
        }
        let binding = WindowBinding(element: nil, id: id, owner: owners[rawPID]!)
        let ax = candidates.first { matchesAXWindow($0, binding, frame: frame, candidates: entries) }
        let onScreen = (entry[kCGWindowIsOnscreen as String] as? Bool) == true
        // Closed NSWindows and internal rendering/menu buffers can retain a CG
        // ID and even stale screenshot pixels. Off-screen entries need a live
        // app-published window; covered windows remain on-screen and need no AX.
        guard onScreen || ax != nil else { continue }
        let title = (entry[kCGWindowName as String] as? String) ?? ax.flatMap { try? attribute($0, "AXTitle", optionalMetadata: true) as? String } ?? ""
        result.append(AppWindow(binding: binding.withElement(ax), frame: frame, title: String(title.prefix(400)), onScreen: onScreen))
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
        guard let id, let window = windows.first(where: { $0.binding.id == id }) else {
            throw ComputerError("WINDOW_NOT_FOUND", "Requested window does not belong to the selected application. List its windows and select an exact ID.")
        }
        return window.binding
    }
    if windows.count == 1 { return windows[0].binding }
    if let focused = try? elementAttribute(AXUIElementCreateApplication(identity.pid), "AXFocusedWindow"),
       let window = windows.first(where: { $0.binding.element.map { CFEqual($0, focused) } == true }) { return window.binding }
    throw ComputerError(windows.isEmpty ? "WINDOW_UNAVAILABLE" : "WINDOW_AMBIGUOUS", "No unique initial window. Use computer_windows and select_app with its window_id.")
}
func validateWindow(_ identity: AppIdentity, _ binding: WindowBinding, expectedSize: CGRect? = nil) throws -> CGRect {
    try requireAX(); try requireUnlocked(); try identity.running(); try binding.owner.validate()
    guard try identity.trustedProcess(binding.owner.pid) == binding.owner,
          let entry = windowInfo().first(where: { ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == binding.id }),
          (entry[kCGWindowOwnerPID as String] as? Int) == Int(binding.owner.pid),
          (entry[kCGWindowLayer as String] as? Int) == 0,
          let frame = cgBounds(entry), frame.width > 1, frame.height > 1,
          frame.minX.isFinite, frame.minY.isFinite, frame.width.isFinite, frame.height.isFinite else {
        throw ComputerError("WINDOW_CHANGED", "Bound WindowServer identity was destroyed or replaced. Select a window again.")
    }
    if let element = binding.element, axWindowID(element) == binding.id,
       (try? attribute(element, "AXMinimized") as? Bool) == true { throw ComputerError("WINDOW_CHANGED", "Bound window is minimized. Restore it and observe again.") }
    if (entry[kCGWindowIsOnscreen as String] as? Bool) != true {
        var live = axWindows(binding.owner.pid)
        if binding.owner.pid != identity.pid { live += axWindows(identity.pid) }
        guard live.contains(where: { matchesAXWindow($0, binding, frame: frame, candidates: windowInfo()) }) else {
            throw ComputerError("WINDOW_CHANGED", "Bound window is no longer displayed and has no live accessibility window. Restore it and select a window again.")
        }
    }
    if let expectedSize, !sameSize(frame, expectedSize) {
        throw ComputerError("STATE_CHANGED", "Bound window was resized after observation. No input was dispatched. Observe again.")
    }
    return frame
}

// Keyboard events still need an unambiguous app-local destination. A canvas
// need not publish AXFocusedUIElement, but a different key window must not get
// input meant for the pinned window. Never change global or app-local focus.
func validateKeyboardWindow(_ identity: AppIdentity, _ binding: WindowBinding, frame: CGRect) throws {
    let app = AXUIElementCreateApplication(binding.owner.pid)
    if let focused = try? elementAttribute(app, "AXFocusedWindow") {
        guard matchesAXWindow(focused, binding, frame: frame, candidates: windowInfo()) else {
            throw ComputerError("FOCUS_CHANGED", "Another window in the selected application has keyboard focus. Observe and select that window if intended.")
        }
        return
    }
    // AppKit retains off-screen menu/rendering surfaces in WindowServer. They
    // cannot become key windows while ordered out. Covered windows still have
    // on_screen=true; this does not require foreground or unobscured pixels.
    let windows = try applicationWindows(identity).filter { $0.binding.owner == binding.owner && $0.onScreen }
    guard windows.count == 1, windows[0].binding.id == binding.id else {
        throw ComputerError("FOCUS_CHANGED", "Cannot determine the keyboard destination among this application's windows. Select a window with app-local keyboard focus.")
    }
}

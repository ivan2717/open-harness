import AppKit
import ApplicationServices
import Darwin
import Foundation

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
        guard let window = binding.element, try belongsToWindow(element, window) else { throw ComputerError("STATE_CHANGED", "Referenced element is no longer inside the bound window. Observe again.") }
        let current = try ElementState.read(element, identity, frame: frame)
        guard current.owner == owner, current.role == role, current.subrole == subrole,
              current.identifier == identifier, current.title == title, current.enabled == enabled,
              !geometry || (box == nil && current.box == nil) || (box != nil && current.box != nil && sameRect(box!, current.box!)) else {
            throw ComputerError("STATE_CHANGED", "Referenced control identity, label, enabled state or position changed. No input was dispatched. Observe again.")
        }
        return current
    }
}

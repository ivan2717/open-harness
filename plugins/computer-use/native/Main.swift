import AppKit
import ApplicationServices
import CryptoKit
import Foundation

@main struct ComputerHelper {
    static func main() {
        let independentApp: Bool
        do { independentApp = try LocalIPC.connectIfRequested() }
        catch { exit(1) }
        // AppKit updates NSRunningApplication/NSWorkspace state on the main run
        // loop. Blocking that loop in readLine leaves activation/PID state stale.
        let app = NSApplication.shared
        app.setActivationPolicy(independentApp ? .accessory : .prohibited)
        let lines = AsyncStream<String> { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                while let line = readLine() { continuation.yield(line) }
                if independentApp { exit(0) } // Host exit/cancellation must not leave an app behind.
                continuation.finish()
            }
        }
        Task { @MainActor in
            await serve(lines)
            exit(0)
        }
        app.run()
    }
    @MainActor static func serve(_ lines: AsyncStream<String>) async {
        var selected: AppIdentity?
        var last: Observation?
        var snapshotID: String?
        var snapshotTime = Date.distantPast
        var screenshotDigest: String?
        for await line in lines {
            var id: Any = NSNull()
            do {
                guard line.utf8.count <= 64 * 1024,
                      let data = line.data(using: .utf8),
                      let request = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                      let method = request["method"] as? String,
                      let params = request["params"] as? [String: Any] else { throw ComputerError("INVALID_REQUEST", "Invalid helper request.") }
                id = request["id"] ?? NSNull()
                var result: [String: Any] = [:]
                switch method {
                case "status":
                    result = ["platform": "macos", "accessibility": AXIsProcessTrusted(), "screen_recording": CGPreflightScreenCaptureAccess(), "selected_app": selected?.json as Any? ?? NSNull(), "permissions_requested": false, "helper_path": URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL.path, "permission_app_name": LocalIPC.displayName, "permission_bundle_id": Bundle.main.bundleIdentifier as Any? ?? NSNull(), "permission_settings": ["accessibility": "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility", "screen_recording": "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"]]
                case "request_accessibility", "request_screen_recording":
                    // Host-only entry points, deliberately absent from the MCP tool catalog.
                    // The desktop UI calls these only after the user clicks a permission button.
                    let permission = method == "request_accessibility" ? "accessibility" : "screen_recording"
                    let granted: Bool
                    if method == "request_accessibility" {
                        let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true]
                        granted = AXIsProcessTrustedWithOptions(options as CFDictionary)
                    } else {
                        granted = CGRequestScreenCaptureAccess()
                    }
                    result = ["permission": permission, "granted": granted, "permissions_requested": true,
                              "helper_path": URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL.path,
                              "settings_url": "x-apple.systempreferences:com.apple.preference.security?" + (permission == "accessibility" ? "Privacy_Accessibility" : "Privacy_ScreenCapture")]
                case "apps":
                    result = ["applications": NSWorkspace.shared.runningApplications.filter { $0.activationPolicy == .regular && !$0.isTerminated }.compactMap { app -> [String: Any]? in
                        guard let bundle = app.bundleIdentifier else { return nil }
                        return ["name": app.localizedName ?? bundle, "bundle_id": bundle, "pid": app.processIdentifier]
                    }]
                case "select_app":
                    selected = nil; last = nil; snapshotID = nil; screenshotDigest = nil
                    try requireAX()
                    guard let bundle = params["bundle_id"] as? String, let pid = params["pid"] as? Int32,
                          let app = NSRunningApplication(processIdentifier: pid), app.bundleIdentifier == bundle, app.activationPolicy == .regular else {
                        throw ComputerError("APP_NOT_FOUND", "Exact bundle ID and PID must match a running regular application.")
                    }
                    let identity = try AppIdentity.read(app)
                    guard app.isActive || app.activate(options: [.activateAllWindows]) else { throw ComputerError("ACTIVATION_FAILED", "Application could not be activated. Unlock the desktop and bring its window forward before selecting it again.") }
                    selected = identity
                    result = ["selected_app": identity.json, "dispatched": true, "verified": false, "next": "Call computer_observe to read the current window."]
                case "observe":
                    guard let identity = selected else { throw ComputerError("APP_NOT_SELECTED", "Select an application first.") }
                    last = nil; snapshotID = nil; screenshotDigest = nil
                    let state = try observe(identity)
                    var captured: [String: Any]?
                    if params["screenshot"] as? Bool == true { captured = try await screenshot(identity, state) }
                    // Observation must describe the same state before and after capture.
                    let verified = try observe(identity)
                    guard verified.fingerprint == state.fingerprint, CFEqual(verified.window, state.window) else { throw ComputerError("STATE_CHANGED", "Window changed during observation. Observe again.") }
                    last = verified; snapshotID = UUID().uuidString; snapshotTime = Date(); screenshotDigest = captured.flatMap { imageDigest($0) }
                    result = verified.json
                    result["snapshot_id"] = snapshotID
                    result["selected_app"] = identity.json
                    result["coordinate_space"] = "window-local points; origin top left"
                    result["expires_in_seconds"] = 60
                    result["untrusted_content"] = true
                    if let captured {
                        result["image"] = ["mimeType": captured["mimeType"]!, "data": captured["data"]!]
                        result["screenshot"] = ["width": captured["width"]!, "height": captured["height"]!, "window_width_points": state.frame.width, "window_height_points": state.frame.height]
                    }
                case "click", "type", "key", "scroll":
                    guard let identity = selected, let previous = last, let token = snapshotID, params["snapshot_id"] as? String == token, Date().timeIntervalSince(snapshotTime) <= 60 else {
                        throw ComputerError("STALE_SNAPSHOT", "Snapshot is missing, consumed, expired, or belongs to another session. Observe again.")
                    }
                    let priorScreenshotDigest = screenshotDigest
                    // Consume before validation/dispatch: an uncertain action must never be repeated blindly.
                    last = nil; snapshotID = nil; screenshotDigest = nil
                    try requireForeground(identity)
                    let current = try observe(identity)
                    guard current.fingerprint == previous.fingerprint, CFEqual(current.window, previous.window),
                          (current.focused == nil && previous.focused == nil) || (current.focused != nil && previous.focused != nil && CFEqual(current.focused!, previous.focused!)) else {
                        throw ComputerError("STATE_CHANGED", "App, window, focus or accessibility state changed. No input was dispatched. Observe again.")
                    }
                    if method == "scroll" || (method == "click" && params["ref"] == nil) {
                        guard let priorScreenshotDigest else { throw ComputerError("SCREENSHOT_REQUIRED", "Coordinate input requires a current screenshot. Observe with screenshot: true.") }
                        let freshImage = try await screenshot(identity, current)
                        guard imageDigest(freshImage) == priorScreenshotDigest else {
                            throw ComputerError("STATE_CHANGED", "Window pixels changed since the screenshot. No input was dispatched. Observe again.")
                        }
                    }
                    if let ref = params["ref"] as? String {
                        guard let oldElement = previous.refs[ref], let currentElement = current.refs[ref], CFEqual(oldElement, currentElement) else {
                            throw ComputerError("STATE_CHANGED", "Referenced accessibility element was replaced. Observe again.")
                        }
                    }
                    try dispatchInput(method, params, current, identity)
                    result = ["dispatched": true, "verified": false, "next": "Observe again and verify the intended result before continuing."]
                default: throw ComputerError("UNKNOWN_METHOD", "Unknown helper operation.")
                }
                emit(["id": id, "result": result])
            } catch let error as ComputerError {
                emit(["id": id, "error": ["code": error.code, "message": error.message]])
            } catch {
                emit(["id": id, "error": ["code": "NATIVE_ERROR", "message": "Native operation failed. Observe again before retrying an action."]])
            }
        }
    }
    static func imageDigest(_ image: [String: Any]) -> String? {
        guard let data = image["data"] as? String else { return nil }
        return SHA256.hash(data: Data(data.utf8)).map { String(format: "%02x", $0) }.joined()
    }
    static func emit(_ value: [String: Any]) {
        if let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), let text = String(data: data, encoding: .utf8) {
            print(text)
            fflush(stdout)
        }
    }
}

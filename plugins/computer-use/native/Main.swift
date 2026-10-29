import AppKit
import ApplicationServices
import Foundation

@main struct ComputerHelper {
    static func main() {
        let independentApp: Bool
        do { independentApp = try LocalIPC.connectIfRequested() }
        catch { exit(1) }
        // AppKit updates NSRunningApplication/NSWorkspace state on the main run
        // loop. Blocking that loop in readLine leaves activation/PID state stale.
        InputCancellation.install()
        let app = NSApplication.shared
        app.setActivationPolicy(independentApp ? .accessory : .prohibited)
        let lines = AsyncStream<String> { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                while let line = readLine() { continuation.yield(line) }
                // Let an in-flight pointer transaction release its held button
                // before the bounded cancellation watchdog terminates the app.
                InputCancellation.cancel()
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
        var selectedWindow: WindowBinding?
        var last: Observation?
        var snapshotID: String?
        var snapshotTime = Date.distantPast
        var screenshotTime: Date?
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
                    result = ["platform": "macos", "capabilities": ["input_scope": "background_window", "window_selection": true, "coordinates_require_screenshot": true, "actions": ["click", "type", "key", "scroll", "drag", "set_value", "action"], "verification": "dispatch_only_except_accessibility_readback", "text_select_all": "accessibility_selection_range", "routing": "pid_and_window", "system_focus_changes": false], "accessibility": AXIsProcessTrusted(), "screen_recording": CGPreflightScreenCaptureAccess(), "selected_app": selected?.json as Any? ?? NSNull(), "selected_window_id": selectedWindow.map { Int($0.id) } as Any? ?? NSNull(), "permissions_requested": false, "helper_path": URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL.path, "permission_app_name": LocalIPC.displayName, "permission_bundle_id": Bundle.main.bundleIdentifier as Any? ?? NSNull(), "permission_settings": ["accessibility": "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility", "screen_recording": "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"]]
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
                    selected = nil; selectedWindow = nil; last = nil; snapshotID = nil; screenshotTime = nil
                    try requireAX(); try requireUnlocked()
                    guard let bundle = params["bundle_id"] as? String, let pid = params["pid"] as? Int32,
                          let app = NSRunningApplication(processIdentifier: pid), app.bundleIdentifier == bundle, app.activationPolicy == .regular else {
                        throw ComputerError("APP_NOT_FOUND", "Exact bundle ID and PID must match a running regular application.")
                    }
                    let identity = try AppIdentity.read(app)
                    selected = identity
                    do {
                        selectedWindow = try selectWindow(identity, requested: params["window_id"])
                    } catch let error as ComputerError {
                        guard params["window_id"] == nil, ["WINDOW_UNAVAILABLE", "WINDOW_AMBIGUOUS"].contains(error.code) else { selected = nil; throw error }
                    }
                    result = ["selected_app": identity.json, "selected_window_id": selectedWindow.map { Int($0.id) } as Any? ?? NSNull(), "activated": false, "next": selectedWindow == nil ? "Call computer_windows, then select_app with an exact window_id." : "Call computer_observe to read the bound window."]
                case "windows":
                    guard let identity = selected else { throw ComputerError("APP_NOT_SELECTED", "Select an application first.") }
                    result = ["selected_app": identity.json, "selected_window_id": selectedWindow.map { Int($0.id) } as Any? ?? NSNull(), "windows": try applicationWindows(identity).map { $0.json }]
                case "observe":
                    guard let identity = selected else { throw ComputerError("APP_NOT_SELECTED", "Select an application first.") }
                    guard let binding = selectedWindow else { throw ComputerError("WINDOW_NOT_SELECTED", "List the selected application's windows and select an exact window_id first.") }
                    last = nil; snapshotID = nil; screenshotTime = nil
                    let state = try observe(identity, binding)
                    var captured: [String: Any]?
                    if params["screenshot"] as? Bool == true { captured = try await screenshot(identity, state) }
                    // Animated content and unrelated accessibility changes do
                    // not invalidate a target. Only the bound window's identity
                    // and dimensions must survive observation/capture.
                    let finalFrame = try validateWindow(identity, binding, expectedSize: state.frame)
                    let verified = state.withFrame(finalFrame)
                    last = verified; snapshotID = UUID().uuidString; snapshotTime = Date()
                    if captured != nil { screenshotTime = snapshotTime }
                    result = verified.json
                    result["snapshot_id"] = snapshotID
                    result["selected_app"] = identity.json
                    result["selected_window_id"] = Int(binding.id)
                    result["coordinate_space"] = "window-local points; origin top left"
                    result["expires_in_seconds"] = 60
                    result["coordinate_expires_in_seconds"] = 15
                    result["untrusted_content"] = true
                    if let captured {
                        result["image"] = ["mimeType": captured["mimeType"]!, "data": captured["data"]!]
                        result["screenshot"] = ["width": captured["width"]!, "height": captured["height"]!, "window_width_points": state.frame.width, "window_height_points": state.frame.height]
                    }
                case "click", "type", "key", "scroll", "drag", "set_value", "action":
                    guard let identity = selected, let previous = last, let token = snapshotID, params["snapshot_id"] as? String == token, Date().timeIntervalSince(snapshotTime) <= 60 else {
                        throw ComputerError("STALE_SNAPSHOT", "Snapshot is missing, consumed, expired, or belongs to another session. Observe again.")
                    }
                    let capturedAt = screenshotTime
                    // Consume before any validation/dispatch; uncertain actions
                    // cannot be replayed using the same observation.
                    last = nil; snapshotID = nil; screenshotTime = nil
                    if requiresScreenshot(method, params) {
                        guard let capturedAt, Date().timeIntervalSince(capturedAt) <= 15 else {
                            throw ComputerError("SCREENSHOT_REQUIRED", "Coordinate input requires a screenshot from the last 15 seconds. Observe with screenshot: true.")
                        }
                    }
                    result = try dispatchInput(method, params, previous, identity)
                default: throw ComputerError("UNKNOWN_METHOD", "Unknown helper operation.")
                }
                emit(["id": id, "result": result])
            } catch let error as ComputerError {
                emit(["id": id, "error": ["code": error.code, "message": error.message, "dispatched": error.dispatched as Any? ?? NSNull(), "recoverable": error.recovery.recoverable, "next": error.recovery.next]])
            } catch {
                emit(["id": id, "error": ["code": "NATIVE_ERROR", "message": "Native operation failed. Observe again before retrying an action.", "dispatched": NSNull(), "recoverable": true]])
            }
        }
    }
    static func emit(_ value: [String: Any]) {
        if let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), let text = String(data: data, encoding: .utf8) {
            print(text)
            fflush(stdout)
        }
    }
}

import AppKit
import Foundation

// Real AX behavior switches, confined to disposable fixture apps. This exercises
// missing AXWindows and a fully unexposed window through the production driver.
var fixtureAXMode = "normal"
final class FixtureApplication: NSApplication {
    override func accessibilityWindows() -> [Any]? { fixtureAXMode == "normal" ? super.accessibilityWindows() : [] }
    override func accessibilityFocusedWindow() -> Any? { fixtureAXMode == "unavailable" ? nil : super.accessibilityFocusedWindow() }
    override func accessibilityMainWindow() -> Any? { fixtureAXMode == "unavailable" ? nil : super.accessibilityMainWindow() }
    override var accessibilityFocusedUIElement: Any? { fixtureAXMode == "unavailable" ? nil : super.accessibilityFocusedUIElement }
}
final class FixtureWindow: NSWindow {
    override func accessibilityFrame() -> NSRect {
        let frame = super.accessibilityFrame()
        return fixtureAXMode == "geometry_mismatch" ? frame.offsetBy(dx: 20, dy: 20) : frame
    }
}
// This disposable app changes only its own windows and responders. It never
// injects global input or touches a user application.
final class InstrumentedTextField: NSTextField {
    var trackingStarted: ((NSEvent) -> Int?)?
    var trackingFinished: ((Int) -> Void)?
    override func mouseDown(with event: NSEvent) {
        let record = trackingStarted?(event)
        defer { if let record { trackingFinished?(record) } }
        // Preserve standard NSTextField/NSCell behavior, including its first
        // mouse policy. The cell may consume mouseUp inside its tracking loop,
        // so the NSApplication local monitor need not see that release.
        super.mouseDown(with: event)
    }
}
final class PointerPad: NSView {
    var changed: (() -> Void)?
    var drags = 0
    var rightClicks = 0
    var middleClicks = 0
    var lastClickCount = 0
    var lastModifiers: UInt = 0
    var typed = ""
    var changedAppearance = false
    var dragStart: NSPoint?
    var dragEnd: NSPoint?
    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func draw(_ dirtyRect: NSRect) {
        (changedAppearance ? NSColor.systemRed : NSColor.systemTeal.withAlphaComponent(0.2)).setFill()
        bounds.fill()
        ("Fixture pointer pad" as NSString).draw(at: NSPoint(x: 12, y: 15), withAttributes: [.foregroundColor: NSColor.labelColor])
    }
    override func mouseDown(with event: NSEvent) {
        lastClickCount = event.clickCount
        lastModifiers = event.modifierFlags.rawValue
        dragStart = convert(event.locationInWindow, from: nil)
        dragEnd = nil
        changed?()
    }
    override func mouseDragged(with event: NSEvent) {
        drags += 1
        dragEnd = convert(event.locationInWindow, from: nil)
        changed?()
    }
    override func mouseUp(with event: NSEvent) {
        dragEnd = convert(event.locationInWindow, from: nil)
        changed?()
    }
    override func rightMouseDown(with event: NSEvent) { rightClicks += 1; changed?() }
    override func otherMouseDown(with event: NSEvent) { middleClicks += 1; changed?() }
    override func keyDown(with event: NSEvent) { typed += event.characters ?? ""; changed?() }
}

final class Fixture: NSObject, NSApplicationDelegate, NSTextFieldDelegate {
    var window: NSWindow!
    var secondWindow: NSWindow?
    let field = InstrumentedTextField(string: "")
    let secret = NSSecureTextField(string: "")
    let secondField = NSTextField(string: "")
    var incrementButton: NSButton!
    let counter = NSTextField(labelWithString: "Clicks: 0")
    let animation = NSTextField(labelWithString: "Frame 0")
    let scroll = NSScrollView()
    let pad = PointerPad()
    var clicks = 0
    var secondClicks = 0
    var ticks = 0
    var ready = false
    var pointerEvents: [[String: Any]] = []
    var keyEvents = 0
    var keyEventDetails: [[String: Any]] = []
    var textMouseTrackingStarted = 0
    var textMouseTrackingFinished = 0
    var textMouseTracking: [[String: Any]] = []
    var activationCount = 0
    var foregroundInputRemaining = 0
    var foregroundInputCount = 0
    var foregroundInputInterrupted = false
    var lastTestCommand = ""
    let output: URL
    let label: String
    init(_ output: URL) {
        self.output = output
        self.label = Bundle.main.object(forInfoDictionaryKey: "CFBundleName") as? String ?? "Fixture"
    }
    func applicationDidFinishLaunching(_ notification: Notification) {
        let menu = NSMenu()
        let edit = NSMenuItem()
        edit.submenu = NSMenu(title: "Edit")
        edit.submenu?.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        menu.addItem(edit)
        NSApp.mainMenu = menu
        window = FixtureWindow(contentRect: NSRect(x: 180, y: 180, width: 560, height: 480), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.title = "\(label) Primary"
        guard let view = window.contentView else { return }
        field.frame = NSRect(x: 24, y: 430, width: 360, height: 26)
        field.placeholderString = "Fixture input"
        field.setAccessibilityIdentifier("fixture-input")
        field.delegate = self
        field.trackingStarted = { [weak self] event in
            guard let self else { return nil }
            let index = self.pointerEvents.count - 1
            self.textMouseTrackingStarted += 1
            let record = self.textMouseTracking.count
            self.textMouseTracking.append(["down_event_index": index, "window": event.windowNumber, "event_number": event.eventNumber, "timestamp": event.timestamp, "x": event.locationInWindow.x, "y": event.locationInWindow.y, "finished": false])
            self.save()
            return record
        }
        field.trackingFinished = { [weak self] record in
            guard let self else { return }
            self.textMouseTrackingFinished += 1
            self.textMouseTracking[record]["finished"] = true
            self.textMouseTracking[record]["end_event_index"] = self.pointerEvents.count
            self.save()
        }
        view.addSubview(field)
        secret.frame = NSRect(x: 400, y: 430, width: 136, height: 26)
        secret.placeholderString = "Password"
        secret.setAccessibilityIdentifier("fixture-secret")
        view.addSubview(secret)
        let button = NSButton(title: "Increment fixture counter", target: self, action: #selector(increment))
        button.frame = NSRect(x: 24, y: 380, width: 240, height: 32)
        button.setAccessibilityIdentifier("fixture-increment")
        view.addSubview(button)
        incrementButton = button
        counter.frame = NSRect(x: 290, y: 385, width: 200, height: 24)
        view.addSubview(counter)
        animation.frame = NSRect(x: 24, y: 340, width: 300, height: 24)
        animation.setAccessibilityIdentifier("fixture-animation")
        view.addSubview(animation)
        pad.frame = NSRect(x: 24, y: 270, width: 512, height: 60)
        pad.setAccessibilityElement(true)
        pad.setAccessibilityRole(.group)
        pad.setAccessibilityLabel("Fixture pointer pad")
        pad.setAccessibilityIdentifier("fixture-pad")
        pad.changed = { [weak self] in self?.save() }
        view.addSubview(pad)
        scroll.frame = NSRect(x: 24, y: 24, width: 512, height: 220)
        scroll.hasVerticalScroller = true
        let text = NSTextView(frame: NSRect(x: 0, y: 0, width: 480, height: 1800))
        text.string = (1...80).map { "Fixture scroll row \($0)" }.joined(separator: "\n")
        text.isEditable = false
        text.setAccessibilityIdentifier("fixture-scroll")
        scroll.documentView = text
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(self, selector: #selector(save), name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        view.addSubview(scroll)
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(field)
        NSApp.activate(ignoringOtherApps: true)
        NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .leftMouseUp, .leftMouseDragged, .rightMouseDown, .rightMouseUp, .otherMouseDown, .otherMouseUp, .scrollWheel, .keyDown, .keyUp]) { [weak self] event in
            if event.type == .keyDown || event.type == .keyUp {
                self?.keyEvents += 1
                self?.keyEventDetails.append(["type": event.type.rawValue, "window": event.windowNumber, "key_code": event.keyCode, "flags": event.modifierFlags.rawValue, "characters": event.characters ?? "", "characters_ignoring_modifiers": event.charactersIgnoringModifiers ?? ""])
            }
            else {
                self?.pointerEvents.append(["type": event.type.rawValue, "window": event.windowNumber, "event_number": event.type == .scrollWheel ? -1 : event.eventNumber, "timestamp": event.timestamp, "x": event.locationInWindow.x, "y": event.locationInWindow.y, "click_count": event.type == .scrollWheel ? 0 : event.clickCount, "modifiers": event.modifierFlags.rawValue])
            }
            self?.save()
            return event
        }
        save()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in self?.ready = true; self?.save() }
        let timer = Timer(timeInterval: 0.05, repeats: true) { [weak self] _ in self?.tick() }
        RunLoop.main.add(timer, forMode: .common)
    }
    @objc func increment() { clicks += 1; counter.stringValue = "Clicks: \(clicks)"; save() }
    @objc func incrementSecond() { secondClicks += 1; save() }
    func controlTextDidChange(_ notification: Notification) { save() }
    func applicationDidBecomeActive(_ notification: Notification) { activationCount += 1; save() }
    func applicationDidResignActive(_ notification: Notification) { save() }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }
    func tick() {
        ticks += 1
        animation.stringValue = "Animation frame \(ticks)"
        animation.textColor = ticks % 2 == 0 ? .systemBlue : .systemOrange
        readTestCommand()
        if foregroundInputRemaining > 0 {
            // Local responder input models typing in foreground B without using
            // the desktop event channel or affecting another application.
            if NSApp.isActive, window.isKeyWindow, let editor = field.currentEditor() {
                editor.insertText("u")
                foregroundInputCount += 1
            } else { foregroundInputInterrupted = true }
            foregroundInputRemaining -= 1
        }
        save()
    }
    func readTestCommand() {
        guard ready,
              let data = try? Data(contentsOf: output.appendingPathExtension("command.json")),
              let value = try? JSONSerialization.jsonObject(with: data) as? [String: String],
              let command = value["command"], let id = value["id"], !id.isEmpty, id.count <= 64,
              id != lastTestCommand else { return }
        lastTestCommand = id
        switch command {
        case "change_canvas": pad.changedAppearance.toggle(); pad.needsDisplay = true; pad.displayIfNeeded()
        case "ax_windows_empty": fixtureAXMode = "windows_empty"
        case "ax_unavailable": fixtureAXMode = "unavailable"
        case "ax_geometry": fixtureAXMode = "geometry_mismatch"
        case "ax_restore": fixtureAXMode = "normal"
        case "resize": window.setContentSize(NSSize(width: 600, height: 500))
        case "move": window.setFrameOrigin(NSPoint(x: window.frame.minX + 20, y: window.frame.minY + 20))
        case "move_button": incrementButton.setFrameOrigin(NSPoint(x: 290, y: 340))
        case "restore_button": incrementButton.setFrameOrigin(NSPoint(x: 24, y: 380))
        case "replace_button":
            let replacement = NSButton(title: "Increment fixture counter", target: self, action: #selector(increment))
            replacement.frame = incrementButton.frame
            replacement.setAccessibilityIdentifier("fixture-increment")
            incrementButton.removeFromSuperview()
            window.contentView?.addSubview(replacement)
            incrementButton = replacement
        case "focus_input": window.makeKey(); window.makeFirstResponder(field)
        case "focus_secret": window.makeKey(); window.makeFirstResponder(secret)
        case "focus_pad": window.makeKey(); window.makeFirstResponder(pad)
        case "activate":
            window.makeKeyAndOrderFront(nil)
            window.makeFirstResponder(field)
            NSApp.activate(ignoringOtherApps: true)
        case "foreground_input":
            window.makeKeyAndOrderFront(nil)
            window.makeFirstResponder(field)
            NSApp.activate(ignoringOtherApps: true)
            foregroundInputRemaining = 60
        case "second_window":
            if secondWindow == nil {
                let created = NSWindow(contentRect: NSRect(x: 210, y: 210, width: 560, height: 480), styleMask: [.titled, .closable], backing: .buffered, defer: false)
                created.isReleasedWhenClosed = false
                created.title = "\(label) Secondary"
                secondField.frame = NSRect(x: 24, y: 430, width: 360, height: 26)
                secondField.setAccessibilityIdentifier("fixture-secondary-input")
                created.contentView?.addSubview(secondField)
                let button = NSButton(title: "Increment secondary counter", target: self, action: #selector(incrementSecond))
                button.frame = NSRect(x: 24, y: 380, width: 260, height: 32)
                button.setAccessibilityIdentifier("fixture-secondary-increment")
                created.contentView?.addSubview(button)
                secondWindow = created
            }
            secondWindow?.orderFront(nil)
            secondWindow?.makeKey()
            secondWindow?.makeFirstResponder(secondField)
        case "close_primary": window.close()
        default: break
        }
    }
    @objc func save() {
        var object: [String: Any] = ["pid": ProcessInfo.processInfo.processIdentifier, "bundle_id": Bundle.main.bundleIdentifier ?? "", "clicks": clicks, "second_clicks": secondClicks, "text": field.stringValue, "secret_length": secret.stringValue.count, "second_text": secondField.stringValue, "scroll_y": scroll.contentView.bounds.origin.y, "pointer_events": pointerEvents, "key_events": keyEvents, "drags": pad.drags, "right_clicks": pad.rightClicks, "middle_clicks": pad.middleClicks, "last_click_count": pad.lastClickCount, "last_modifiers": pad.lastModifiers, "last_test_command": lastTestCommand, "animation_frame": ticks, "foreground_input_count": foregroundInputCount, "foreground_input_remaining": foregroundInputRemaining, "foreground_input_interrupted": foregroundInputInterrupted, "activation_count": activationCount, "ready": ready, "active": NSApp.isActive, "finished": NSRunningApplication.current.isFinishedLaunching, "window_id": window?.windowNumber ?? 0, "second_window_id": secondWindow?.windowNumber ?? 0, "window_visible": window?.isVisible ?? false, "window_key": window?.isKeyWindow ?? false, "foreground_bundle": NSWorkspace.shared.frontmostApplication?.bundleIdentifier ?? "none"]
        if let point = pad.dragStart { object["drag_start"] = ["x": point.x, "y": point.y] }
        if let point = pad.dragEnd { object["drag_end"] = ["x": point.x, "y": point.y] }
        object["key_event_details"] = keyEventDetails
        object["ax_mode"] = fixtureAXMode
        object["canvas_text"] = pad.typed
        object["text_mouse_tracking_started"] = textMouseTrackingStarted
        object["text_mouse_tracking_finished"] = textMouseTrackingFinished
        object["text_mouse_tracking"] = textMouseTracking
        if let data = try? JSONSerialization.data(withJSONObject: object) { try? data.write(to: output, options: .atomic) }
    }
}
let app = FixtureApplication.shared
app.setActivationPolicy(.regular)
let fixture = Fixture(URL(fileURLWithPath: CommandLine.arguments[1]))
app.delegate = fixture
app.run()

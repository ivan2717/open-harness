import AppKit
import Foundation

final class Fixture: NSObject, NSApplicationDelegate, NSTextFieldDelegate {
    var window: NSWindow!
    let field = NSTextField(string: "")
    let counter = NSTextField(labelWithString: "Clicks: 0")
    let scroll = NSScrollView()
    var clicks = 0
    var ready = false
    var pointerEvents: [[String: Any]] = []
    var keyEvents = 0
    var lastTestCommand = ""
    let output: URL
    init(_ output: URL) { self.output = output }
    func applicationDidFinishLaunching(_ notification: Notification) {
        window = NSWindow(contentRect: NSRect(x: 180, y: 180, width: 520, height: 420), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.title = "Open Harness Computer Use Test Fixture"
        guard let view = window.contentView else { return }
        field.frame = NSRect(x: 24, y: 350, width: 460, height: 26)
        field.placeholderString = "Fixture input"
        field.setAccessibilityIdentifier("fixture-input")
        field.delegate = self
        view.addSubview(field)
        let button = NSButton(title: "Increment fixture counter", target: self, action: #selector(increment))
        button.frame = NSRect(x: 24, y: 300, width: 240, height: 32)
        button.setAccessibilityIdentifier("fixture-increment")
        view.addSubview(button)
        counter.frame = NSRect(x: 290, y: 305, width: 170, height: 24)
        view.addSubview(counter)
        let resize = NSButton(title: "Resize fixture", target: self, action: #selector(resizeWindow))
        resize.frame = NSRect(x: 24, y: 260, width: 180, height: 30)
        resize.setAccessibilityIdentifier("fixture-resize")
        view.addSubview(resize)
        scroll.frame = NSRect(x: 24, y: 24, width: 460, height: 220)
        scroll.hasVerticalScroller = true
        let text = NSTextView(frame: NSRect(x: 0, y: 0, width: 430, height: 1800))
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
        NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .leftMouseUp, .scrollWheel, .keyDown, .keyUp]) { [weak self] event in
            if event.type == .keyDown || event.type == .keyUp { self?.keyEvents += 1 }
            else { self?.pointerEvents.append(["type": event.type.rawValue, "window": event.windowNumber, "x": event.locationInWindow.x, "y": event.locationInWindow.y]) }
            self?.save()
            return event
        }
        save()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in
            self?.ready = true
            self?.save()
        }
        // Only this disposable fixture reads this test-owned file. Resizing it
        // out of band must not launch another automation helper or send input.
        let timer = Timer(timeInterval: 0.05, repeats: true) { [weak self] _ in self?.readTestCommand() }
        RunLoop.main.add(timer, forMode: .common)
    }
    @objc func increment() { clicks += 1; counter.stringValue = "Clicks: \(clicks)"; save() }
    @objc func resizeWindow() { window.setContentSize(NSSize(width: 560, height: 440)); save() }
    func controlTextDidChange(_ notification: Notification) { save() }
    func applicationDidBecomeActive(_ notification: Notification) { save() }
    func applicationDidResignActive(_ notification: Notification) { save() }
    func readTestCommand() {
        guard ready,
              let data = try? Data(contentsOf: output.appendingPathExtension("command.json")),
              let value = try? JSONSerialization.jsonObject(with: data) as? [String: String],
              value["command"] == "resize", let id = value["id"], !id.isEmpty, id.count <= 64,
              id != lastTestCommand else { return }
        lastTestCommand = id
        // A blinking text caret is intentional pixel drift, unrelated to the
        // stale-window check or the subsequent screenshot/scroll assertions.
        window.makeFirstResponder(nil)
        resizeWindow()
    }
    @objc func save() {
        let object: [String: Any] = ["pid": ProcessInfo.processInfo.processIdentifier, "clicks": clicks, "text": field.stringValue, "scroll_y": scroll.contentView.bounds.origin.y, "pointer_events": pointerEvents, "key_events": keyEvents, "last_test_command": lastTestCommand, "ready": ready, "active": NSApp.isActive, "finished": NSRunningApplication.current.isFinishedLaunching, "window_visible": window?.isVisible ?? false, "window_key": window?.isKeyWindow ?? false, "foreground_bundle": NSWorkspace.shared.frontmostApplication?.bundleIdentifier ?? "none"]
        if let data = try? JSONSerialization.data(withJSONObject: object) { try? data.write(to: output, options: .atomic) }
    }
}
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let fixture = Fixture(URL(fileURLWithPath: CommandLine.arguments[1]))
app.delegate = fixture
app.run()

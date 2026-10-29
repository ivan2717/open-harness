using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Windows.Automation;

namespace OpenHarness.ComputerUse {
    internal static class Input {
        private static readonly object EmergencyLock = new object();
        private static Native.Input[] pendingReleases;
        internal static volatile bool DeliveryAttempted;
        private static readonly Dictionary<string, ushort> Keys = new Dictionary<string, ushort> {
            { "enter", 13 }, { "tab", 9 }, { "escape", 27 }, { "backspace", 8 }, { "delete", 46 },
            { "left", 37 }, { "right", 39 }, { "up", 38 }, { "down", 40 }, { "home", 36 }, { "end", 35 }, { "pageup", 33 }, { "pagedown", 34 }, { "space", 32 }
        };
        internal static void EmergencyRelease() {
            lock (EmergencyLock) {
                if (pendingReleases != null) Native.SendInput((uint)pendingReleases.Length, pendingReleases, Marshal.SizeOf(typeof(Native.Input)));
                pendingReleases = null;
            }
        }
        internal static void IdleInput() {
            foreach (int key in new[] { 1, 2, 4, 16, 17, 18, 91, 92 })
                if ((Native.GetAsyncKeyState(key) & 0x8000) != 0) throw new ComputerError("INPUT_BUSY", "A mouse button or modifier is held. Release it before the next computer action.");
        }
        private static void ForegroundNow(AppIdentity identity) {
            Native.RequireDesktop();
            if (Native.GetForegroundWindow() != identity.Window || Native.WindowProcess(identity.Window) != identity.Pid) throw new ComputerError("FOCUS_CHANGED", "The selected foreground window changed before this input batch.");
        }
        private static void Send(Native.Input[] events, Native.Input[] releases, AppIdentity identity, Action checkTarget) {
            identity.RequireForeground();
            // Identity validation can call a remote UIA provider. Recheck the
            // actual input target afterwards, immediately before OS injection.
            checkTarget(); ForegroundNow(identity); IdleInput();
            // Serialize cancellation cleanup with the short native injection so
            // EOF cannot release modifiers just before this batch presses them.
            lock (EmergencyLock) {
                pendingReleases = releases;
                DeliveryAttempted = true;
                uint count = Native.SendInput((uint)events.Length, events, Marshal.SizeOf(typeof(Native.Input)));
                if (count != events.Length) {
                    EmergencyRelease();
                    throw new ComputerError("INPUT_UNCERTAIN", "Windows did not accept the complete input sequence. Observe before continuing; input is never retried.");
                }
                pendingReleases = null;
            }
        }
        private static Native.Input Key(ushort code, ushort scan, uint flags) {
            return new Native.Input { Type = 1, Data = new Native.InputUnion { Key = new Native.KeyInput { Key = code, Scan = scan, Flags = flags } } };
        }
        private static Native.Input Mouse(uint flags, int x, int y, uint data) {
            return new Native.Input { Type = 0, Data = new Native.InputUnion { Mouse = new Native.MouseInput { X = x, Y = y, Data = data, Flags = flags } } };
        }
        private static void Focus(Observation state, AppIdentity identity) {
            AutomationElement current = AutomationElement.FocusedElement;
            if (state.Focused == null || current == null || Observation.Runtime(current) != state.FocusRuntime) throw new ComputerError("FOCUS_CHANGED", "The focused control changed. Observe again before input.");
            Observation.RequireSafeElement(current, identity);
            AutomationElement latest = AutomationElement.FocusedElement;
            if (latest == null || Observation.Runtime(latest) != state.FocusRuntime) throw new ComputerError("FOCUS_CHANGED", "The focused control changed before this input batch.");
            if (latest.Current.IsPassword) throw new ComputerError("SECURE_FIELD", "Password controls require direct user input.");
        }
        private static Native.Point Point(Dictionary<string, object> args, Observation state, AppIdentity identity) {
            return Point(Arguments.Number(args, "x"), Arguments.Number(args, "y"), state, identity);
        }
        private static Native.Point Point(double x, double y, Observation state, AppIdentity identity) {
            if (!state.HasScreenshot) throw new ComputerError("SCREENSHOT_REQUIRED", "Pointer input requires computer_observe with screenshot: true.");
            state.RefreshWindow(identity);
            if (x < 0 || y < 0 || x >= state.Frame.Width || y >= state.Frame.Height) throw new ComputerError("INVALID_COORDINATES", "Use window-local physical pixels inside the observed window, not scaled screenshot pixels.");
            var point = new Native.Point(state.Frame.Left + (int)Math.Floor(x), state.Frame.Top + (int)Math.Floor(y));
            ExposedPoint(point, identity);
            AutomationElement target = AutomationElement.FromPoint(new System.Windows.Point(point.X, point.Y));
            Observation.RequireSafeElement(target, identity);
            state.RequireObservedPoint(x, y, target);
            ExposedPoint(point, identity);
            return point;
        }
        private static void ExposedPoint(Native.Point point, AppIdentity identity) {
            IntPtr hit = Native.WindowFromPoint(point);
            if (hit == IntPtr.Zero || Native.GetAncestor(hit, 2) != identity.Window || Native.WindowProcess(hit) != identity.Pid) throw new ComputerError("TARGET_OBSCURED", "The point is obscured or outside the selected window. No input was dispatched.");
        }
        private static void Scroll(Dictionary<string, object> args, Observation state, AppIdentity identity) {
            int delta = Arguments.Integer(args, "delta_y", -1000, 1000);
            Native.Point point = Point(args, state, identity);
            if (delta == 0) return;
            AutomationElement target = AutomationElement.FromPoint(new System.Windows.Point(point.X, point.Y));
            var visited = new HashSet<string>();
            for (int depth = 0; target != null && depth < 32; depth++) {
                Observation.RequireSafeElement(target, identity);
                string runtime = Observation.Runtime(target);
                if (!visited.Add(runtime)) throw new ComputerError("TARGET_UNAVAILABLE", "The scroll target's accessibility ancestry is not stable.");
                object value;
                if (target.TryGetCurrentPattern(ScrollPattern.Pattern, out value)) {
                    var pattern = (ScrollPattern)value;
                    if (pattern.Current.VerticallyScrollable) {
                        // UIA exposes semantic increments rather than pixel or
                        // wheel distances. Dispatch one increment without focus
                        // changes, mouse movement or an implicit second attempt.
                        bool large = Math.Abs(delta) >= 360;
                        ScrollAmount amount = delta > 0
                            ? (large ? ScrollAmount.LargeDecrement : ScrollAmount.SmallDecrement)
                            : (large ? ScrollAmount.LargeIncrement : ScrollAmount.SmallIncrement);
                        identity.RequireForeground();
                        SamePoint(point, Point(args, state, identity));
                        Observation.RequireSafeElement(target, identity);
                        if (Observation.Runtime(target) != runtime) throw new ComputerError("STATE_CHANGED", "The scroll container changed before input.");
                        ForegroundNow(identity); IdleInput(); ExposedPoint(point, identity);
                        DeliveryAttempted = true;
                        pattern.Scroll(ScrollAmount.NoAmount, amount);
                        return;
                    }
                }
                if (runtime == identity.WindowRuntime) break;
                target = TreeWalker.RawViewWalker.GetParent(target);
            }
            // Traditional controls may not expose ScrollPattern. Send precisely
            // one wheel message to the hit HWND; DefWindowProc can propagate it
            // within its parent chain. SendInput would use the user's global
            // focus/hover wheel setting and could scroll a different control.
            if (point.X < Int16.MinValue || point.X > Int16.MaxValue || point.Y < Int16.MinValue || point.Y > Int16.MaxValue) throw new ComputerError("INVALID_COORDINATES", "This control requires wheel-message coordinates within the signed 16-bit screen range.");
            identity.RequireForeground();
            SamePoint(point, Point(args, state, identity));
            ForegroundNow(identity); IdleInput();
            IntPtr window = Native.WindowFromPoint(point);
            if (window == IntPtr.Zero || Native.GetAncestor(window, 2) != identity.Window || Native.WindowProcess(window) != identity.Pid) throw new ComputerError("TARGET_OBSCURED", "The wheel target is no longer inside the selected window.");
            uint position = unchecked((uint)(ushort)(short)point.X | ((uint)(ushort)(short)point.Y << 16));
            uint wheel = unchecked((uint)(ushort)(short)delta << 16);
            UIntPtr ignored;
            // SMTO_BLOCK | SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT; never retry a
            // timed-out dispatch because the target may already have acted.
            DeliveryAttempted = true;
            if (Native.SendMessageTimeout(window, 0x020A, new UIntPtr(wheel), new IntPtr(unchecked((int)position)), 0x23, 1500, out ignored) == IntPtr.Zero)
                throw new ComputerError("INPUT_UNCERTAIN", "The target did not acknowledge the wheel message. It may already have scrolled. Observe before continuing; this action is not retried.");
        }
        internal static void ParseKey(string value, out ushort code, out List<string> modifiers) {
            var parts = value.ToLowerInvariant().Split('+');
            modifiers = new List<string>();
            for (int i = 0; i < parts.Length - 1; i++) {
                string part = parts[i];
                if (part == "ctrl") part = "control";
                if (part == "cmd" || part == "command") part = "meta";
                if (part == "option") part = "alt";
                if ((part != "shift" && part != "control" && part != "alt" && part != "meta") || modifiers.Contains(part)) throw new ComputerError("INVALID_KEY", "Invalid or repeated key modifier.");
                modifiers.Add(part);
            }
            string key = parts[parts.Length - 1];
            if (!Keys.TryGetValue(key, out code)) {
                int function;
                if (key.Length == 1 && (key[0] >= 'a' && key[0] <= 'z' || key[0] >= '0' && key[0] <= '9')) code = (ushort)Char.ToUpperInvariant(key[0]);
                else if (key.StartsWith("f") && Int32.TryParse(key.Substring(1), out function) && function >= 1 && function <= 12 && key == "f" + function) code = (ushort)(111 + function);
                else throw new ComputerError("INVALID_KEY", "Unsupported key name.");
            }
            RequireModifiers(modifiers);
            if ((key == "tab" && modifiers.Contains("alt")) || (key == "escape" && (modifiers.Contains("control") || modifiers.Contains("alt"))) ||
                (key == "delete" && modifiers.Contains("control") && modifiers.Contains("alt")) || (key == "f4" && modifiers.Contains("alt")))
                throw new ComputerError("INVALID_KEY", "System and application-switching shortcuts are not supported.");
        }
        private static void RequireModifiers(List<string> modifiers) {
            // Windows logo-key combinations are routed by the shell, so they
            // cannot be confined to this foreground application's window.
            if (modifiers.Contains("meta")) throw new ComputerError("ACTION_UNSUPPORTED", "Windows logo-key shortcuts cannot be confined to the selected window. Use an application shortcut with control, alt, or shift.");
        }
        private static ushort ModifierCode(string modifier) {
            if (modifier == "shift") return 16;
            if (modifier == "control") return 17;
            if (modifier == "alt") return 18;
            return 91;
        }
        private static void BeginModifiers(List<string> modifiers, List<Native.Input> events, List<Native.Input> releases) {
            RequireModifiers(modifiers);
            foreach (string modifier in modifiers) events.Add(Key(ModifierCode(modifier), 0, 0));
            for (int i = modifiers.Count - 1; i >= 0; i--) releases.Add(Key(ModifierCode(modifiers[i]), 0, 2));
        }
        private static Native.Input Move(Native.Point point) {
            int left = Native.GetSystemMetrics(76), top = Native.GetSystemMetrics(77), width = Native.GetSystemMetrics(78), height = Native.GetSystemMetrics(79);
            if (width <= 1 || height <= 1 || point.X < left || point.Y < top || point.X >= left + width || point.Y >= top + height) throw new ComputerError("INVALID_COORDINATES", "The observed point is outside the virtual display.");
            int x = (int)Math.Round((point.X - left) * 65535.0 / (width - 1));
            int y = (int)Math.Round((point.Y - top) * 65535.0 / (height - 1));
            return Mouse(0xC001, x, y, 0);
        }
        private static void SamePoint(Native.Point expected, Native.Point actual) {
            if (expected.X != actual.X || expected.Y != actual.Y) throw new ComputerError("STATE_CHANGED", "The window moved while preparing input. Observe again.");
        }
        private static Action AccessibilityAction(AutomationElement element, string action) {
            object pattern;
            if (action == "Invoke" && element.TryGetCurrentPattern(InvokePattern.Pattern, out pattern)) { var value = (InvokePattern)pattern; return delegate { value.Invoke(); }; }
            if (action == "Toggle" && element.TryGetCurrentPattern(TogglePattern.Pattern, out pattern)) { var value = (TogglePattern)pattern; return delegate { value.Toggle(); }; }
            if (action == "Select" && element.TryGetCurrentPattern(SelectionItemPattern.Pattern, out pattern)) { var value = (SelectionItemPattern)pattern; return delegate { value.Select(); }; }
            if ((action == "Expand" || action == "Collapse") && element.TryGetCurrentPattern(ExpandCollapsePattern.Pattern, out pattern)) {
                var value = (ExpandCollapsePattern)pattern;
                var state = value.Current.ExpandCollapseState;
                if (action == "Expand" && (state == ExpandCollapseState.Collapsed || state == ExpandCollapseState.PartiallyExpanded)) return delegate { value.Expand(); };
                if (action == "Collapse" && (state == ExpandCollapseState.Expanded || state == ExpandCollapseState.PartiallyExpanded)) return delegate { value.Collapse(); };
            }
            if (action == "SetFocus" && element.Current.IsKeyboardFocusable) return delegate { element.SetFocus(); };
            throw new ComputerError("ACTION_UNSUPPORTED", "The observed accessibility action is no longer supported. Observe again.");
        }
        private static void RefAction(string method, Dictionary<string, object> args, Observation state, AppIdentity identity) {
            string reference = Arguments.String(args, "ref", 256);
            AutomationElement element = state.Reference(reference, identity);
            Action dispatch;
            if (method == "set_value") {
                string value = Arguments.String(args, "value", 2000, true); object raw;
                if (!state.Settable.Contains(reference) || !element.TryGetCurrentPattern(ValuePattern.Pattern, out raw)) throw new ComputerError("ACTION_UNSUPPORTED", "The observed target has no writable text value.");
                var pattern = (ValuePattern)raw;
                if (pattern.Current.IsReadOnly) throw new ComputerError("ACTION_UNSUPPORTED", "The target's value is now read-only.");
                dispatch = delegate { pattern.SetValue(value); };
            } else {
                string action = method == "action" ? Arguments.String(args, "action", 256) : null;
                var supported = state.Actions[reference];
                if (action == null) {
                    foreach (string candidate in new[] { "Invoke", "Toggle", "Select", "SetFocus" }) if (supported.Contains(candidate)) { action = candidate; break; }
                }
                if (action == null || !supported.Contains(action)) throw new ComputerError("ACTION_UNSUPPORTED", "Use an action listed in this ref's observed actions array, or observe a screenshot for pointer input.");
                dispatch = AccessibilityAction(element, action);
            }
            identity.RequireForeground(); state.RefreshWindow(identity); state.Reference(reference, identity);
            ForegroundNow(identity); IdleInput();
            DeliveryAttempted = true; dispatch();
        }
        internal static void Dispatch(string method, Dictionary<string, object> args, Observation state, AppIdentity identity) {
            identity.RequireForeground(); IdleInput();
            if (method == "set_value" || method == "action") { RefAction(method, args, state, identity); return; }
            if (method == "click") {
                string button = args.ContainsKey("button") ? Arguments.String(args, "button", 8) : "left";
                if (button != "left" && button != "right" && button != "middle") throw Arguments.Invalid();
                int count = args.ContainsKey("click_count") ? Arguments.Integer(args, "click_count", 1, 3) : 1;
                var modifiers = Arguments.Modifiers(args); RequireModifiers(modifiers);
                if (args.ContainsKey("ref") && button == "left" && count == 1 && modifiers.Count == 0) { RefAction(method, args, state, identity); return; }
                double x, y;
                if (args.ContainsKey("ref")) {
                    string reference = Arguments.String(args, "ref", 256);
                    var element = state.Reference(reference, identity);
                    System.Windows.Rect bounds;
                    if (!state.LocalBounds.TryGetValue(reference, out bounds) || bounds.IsEmpty) throw new ComputerError("TARGET_UNAVAILABLE", "The observed ref has no pointer bounds.");
                    x = bounds.X + bounds.Width / 2; y = bounds.Y + bounds.Height / 2;
                    var current = element.Current.BoundingRectangle;
                    if (current.IsEmpty || !current.Contains(state.Frame.Left + x, state.Frame.Top + y)) throw new ComputerError("TARGET_CHANGED", "The referenced pointer target moved within the window. Observe again.");
                } else { x = Arguments.Number(args, "x"); y = Arguments.Number(args, "y"); }
                Native.Point point = Point(x, y, state, identity);
                uint down = button == "left" ? 2u : button == "right" ? 8u : 32u;
                uint up = down * 2;
                var events = new List<Native.Input>(); var releases = new List<Native.Input>();
                BeginModifiers(modifiers, events, releases); events.Add(Move(point));
                for (int i = 0; i < count; i++) { events.Add(Mouse(down, 0, 0, 0)); events.Add(Mouse(up, 0, 0, 0)); }
                events.AddRange(releases); releases.Insert(0, Mouse(up, 0, 0, 0));
                Send(events.ToArray(), releases.ToArray(), identity, delegate { SamePoint(point, Point(x, y, state, identity)); });
                return;
            }
            if (method == "drag") {
                double fromX = Arguments.Number(args, "from_x"), fromY = Arguments.Number(args, "from_y");
                double toX = Arguments.Number(args, "to_x"), toY = Arguments.Number(args, "to_y");
                var modifiers = Arguments.Modifiers(args); RequireModifiers(modifiers);
                Native.Point from = Point(fromX, fromY, state, identity), to = Point(toX, toY, state, identity);
                var events = new List<Native.Input>(); var releases = new List<Native.Input>();
                BeginModifiers(modifiers, events, releases); events.Add(Move(from)); events.Add(Mouse(2, 0, 0, 0));
                for (int step = 1; step <= 16; step++) {
                    var point = new Native.Point(from.X + (int)Math.Round((to.X - from.X) * step / 16.0), from.Y + (int)Math.Round((to.Y - from.Y) * step / 16.0));
                    ExposedPoint(point, identity); events.Add(Move(point));
                }
                events.Add(Mouse(4, 0, 0, 0)); events.AddRange(releases); releases.Insert(0, Mouse(4, 0, 0, 0));
                Send(events.ToArray(), releases.ToArray(), identity, delegate {
                    SamePoint(from, Point(fromX, fromY, state, identity)); SamePoint(to, Point(toX, toY, state, identity));
                    for (int step = 1; step < 16; step++) ExposedPoint(new Native.Point(from.X + (int)Math.Round((to.X - from.X) * step / 16.0), from.Y + (int)Math.Round((to.Y - from.Y) * step / 16.0)), identity);
                });
                return;
            }
            if (method == "type") {
                string text = Arguments.String(args, "text", 2000);
                for (int offset = 0; offset < text.Length;) {
                    var events = new List<Native.Input>(); var releases = new List<Native.Input>();
                    int end = Math.Min(offset + 16, text.Length);
                    if (end < text.Length && Char.IsHighSurrogate(text[end - 1])) end--;
                    for (; offset < end; offset++) {
                        events.Add(Key(0, text[offset], 4)); events.Add(Key(0, text[offset], 6)); releases.Add(Key(0, text[offset], 6));
                    }
                    Send(events.ToArray(), releases.ToArray(), identity, delegate { state.RefreshWindow(identity); Focus(state, identity); });
                }
                return;
            }
            if (method == "key") {
                Focus(state, identity);
                ushort code; List<string> modifiers;
                ParseKey(Arguments.String(args, "key", 80), out code, out modifiers);
                uint extended = code >= 33 && code <= 46 ? 1u : 0u;
                var events = new List<Native.Input>(); var releases = new List<Native.Input>();
                BeginModifiers(modifiers, events, releases);
                Native.Input up = Key(code, 0, extended | 2);
                events.Add(Key(code, 0, extended)); events.Add(up); events.AddRange(releases); releases.Insert(0, up);
                Send(events.ToArray(), releases.ToArray(), identity, delegate { state.RefreshWindow(identity); Focus(state, identity); });
                return;
            }
            if (method == "scroll") { Scroll(args, state, identity); return; }
            throw new ComputerError("UNKNOWN_METHOD", "Unknown input operation.");
        }
    }
}

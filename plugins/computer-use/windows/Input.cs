using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Windows.Automation;

namespace OpenHarness.ComputerUse {
    internal static class Input {
        private static readonly object EmergencyLock = new object();
        private static Native.Input[] pendingReleases;
        private static readonly Dictionary<string, ushort> Keys = new Dictionary<string, ushort> {
            { "enter", 13 }, { "tab", 9 }, { "escape", 27 }, { "backspace", 8 }, { "delete", 46 },
            { "left", 37 }, { "right", 39 }, { "up", 38 }, { "down", 40 }, { "home", 36 }, { "end", 35 }, { "pageup", 33 }, { "pagedown", 34 }
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
            lock (EmergencyLock) { pendingReleases = releases; }
            uint count = Native.SendInput((uint)events.Length, events, Marshal.SizeOf(typeof(Native.Input)));
            if (count != events.Length) {
                EmergencyRelease();
                throw new ComputerError("INPUT_UNCERTAIN", "Windows did not accept the complete input sequence. Observe before continuing; input is never retried.");
            }
            lock (EmergencyLock) { pendingReleases = null; }
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
            double x = Arguments.Number(args, "x"), y = Arguments.Number(args, "y");
            if (x < 0 || y < 0 || x >= state.Frame.Width || y >= state.Frame.Height) throw new ComputerError("INVALID_COORDINATES", "Use window-local physical pixels inside the observed window, not scaled screenshot pixels.");
            var point = new Native.Point(state.Frame.Left + (int)Math.Floor(x), state.Frame.Top + (int)Math.Floor(y));
            ExposedPoint(point, identity);
            AutomationElement target = AutomationElement.FromPoint(new System.Windows.Point(point.X, point.Y));
            Observation.RequireSafeElement(target, identity);
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
                        Point(args, state, identity);
                        Observation.RequireSafeElement(target, identity);
                        if (Observation.Runtime(target) != runtime) throw new ComputerError("STATE_CHANGED", "The scroll container changed before input.");
                        ForegroundNow(identity); IdleInput(); ExposedPoint(point, identity);
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
            Point(args, state, identity);
            ForegroundNow(identity); IdleInput();
            IntPtr window = Native.WindowFromPoint(point);
            if (window == IntPtr.Zero || Native.GetAncestor(window, 2) != identity.Window || Native.WindowProcess(window) != identity.Pid) throw new ComputerError("TARGET_OBSCURED", "The wheel target is no longer inside the selected window.");
            uint position = unchecked((uint)(ushort)(short)point.X | ((uint)(ushort)(short)point.Y << 16));
            uint wheel = unchecked((uint)(ushort)(short)delta << 16);
            UIntPtr ignored;
            // SMTO_BLOCK | SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT; never retry a
            // timed-out dispatch because the target may already have acted.
            if (Native.SendMessageTimeout(window, 0x020A, new UIntPtr(wheel), new IntPtr(unchecked((int)position)), 0x23, 1500, out ignored) == IntPtr.Zero)
                throw new ComputerError("INPUT_UNCERTAIN", "The target did not acknowledge the wheel message. It may already have scrolled. Observe before continuing; this action is not retried.");
        }
        internal static void Dispatch(string method, Dictionary<string, object> args, Observation state, AppIdentity identity) {
            identity.RequireForeground(); IdleInput();
            if (method == "click" && args.ContainsKey("ref")) {
                string reference = Arguments.String(args, "ref", 256);
                AutomationElement element;
                if (!state.Refs.TryGetValue(reference, out element)) throw new ComputerError("INVALID_REF", "The ref is absent from the latest observation.");
                Observation.RequireSafeElement(element, identity);
                object pattern;
                // Ref operations address a verified element directly; unsupported
                // controls never fall back to a guessed coordinate click.
                Action click;
                if (element.TryGetCurrentPattern(InvokePattern.Pattern, out pattern)) { var invoke = (InvokePattern)pattern; click = delegate { invoke.Invoke(); }; }
                else if (element.TryGetCurrentPattern(TogglePattern.Pattern, out pattern)) { var toggle = (TogglePattern)pattern; click = delegate { toggle.Toggle(); }; }
                else if (element.TryGetCurrentPattern(SelectionItemPattern.Pattern, out pattern)) { var selection = (SelectionItemPattern)pattern; click = delegate { selection.Select(); }; }
                else if (element.Current.ControlType == ControlType.Edit && element.Current.IsKeyboardFocusable) click = delegate { element.SetFocus(); };
                else throw new ComputerError("ACTION_UNSUPPORTED", "This control has no supported accessibility click action. Observe with a screenshot before using coordinates.");
                identity.RequireForeground(); Observation.RequireSafeElement(element, identity);
                if (Observation.Runtime(element) != state.RuntimeIds[reference]) throw new ComputerError("STATE_CHANGED", "The accessibility target was replaced before input.");
                ForegroundNow(identity); IdleInput(); click();
                return;
            }
            if (method == "type") {
                string text = Arguments.String(args, "text", 2000);
                bool dispatched = false;
                try {
                    for (int offset = 0; offset < text.Length;) {
                        var events = new List<Native.Input>(); var releases = new List<Native.Input>();
                        int end = Math.Min(offset + 16, text.Length);
                        if (end < text.Length && Char.IsHighSurrogate(text[end - 1])) end--;
                        for (; offset < end; offset++) {
                            events.Add(Key(0, text[offset], 4)); events.Add(Key(0, text[offset], 6)); releases.Add(Key(0, text[offset], 6));
                        }
                        Send(events.ToArray(), releases.ToArray(), identity, delegate { Focus(state, identity); });
                        dispatched = true;
                    }
                } catch {
                    if (dispatched) throw new ComputerError("INPUT_UNCERTAIN", "Text input stopped after one or more batches. Some text may already be present. Observe and verify before continuing; do not resend the whole text blindly.");
                    throw;
                }
                return;
            }
            if (method == "key") {
                Focus(state, identity);
                string name = Arguments.String(args, "key", 32); ushort code;
                if (!Keys.TryGetValue(name, out code)) throw new ComputerError("INVALID_KEY", "Only supported navigation and editing keys are allowed.");
                uint extended = code >= 33 && code <= 46 ? 1u : 0u;
                Native.Input up = Key(code, 0, extended | 2);
                Send(new[] { Key(code, 0, extended), up }, new[] { up }, identity, delegate { Focus(state, identity); });
                return;
            }
            if (method == "scroll") { Scroll(args, state, identity); return; }
            if (method != "click") throw new ComputerError("UNKNOWN_METHOD", "Unknown input operation.");
            Native.Point point = Point(args, state, identity);
            int left = Native.GetSystemMetrics(76), top = Native.GetSystemMetrics(77), width = Native.GetSystemMetrics(78), height = Native.GetSystemMetrics(79);
            if (width <= 1 || height <= 1 || point.X < left || point.Y < top || point.X >= left + width || point.Y >= top + height) throw new ComputerError("INVALID_COORDINATES", "The observed point is outside the virtual display.");
            int absoluteX = (int)Math.Round((point.X - left) * 65535.0 / (width - 1));
            int absoluteY = (int)Math.Round((point.Y - top) * 65535.0 / (height - 1));
            Native.Input move = Mouse(0xC001, absoluteX, absoluteY, 0); // ABSOLUTE | VIRTUALDESK | MOVE
            Action checkPoint = delegate { Point(args, state, identity); ExposedPoint(point, identity); };
            Native.Input mouseUp = Mouse(4, 0, 0, 0);
            Send(new[] { move, Mouse(2, 0, 0, 0), mouseUp }, new[] { mouseUp }, identity, checkPoint);
        }
    }
}

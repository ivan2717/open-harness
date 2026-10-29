using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace OpenHarness.ComputerUse {
    internal sealed class Session {
        private AppIdentity selected;
        private Observation last;
        private string snapshotId;
        private long observedAt;
        internal const string Coordinates = "window-local physical pixels; origin top left";
        private void ClearSnapshot() { last = null; snapshotId = null; }

        internal object Handle(string method, Dictionary<string, object> args) {
            if (method == "status") {
                Arguments.Keys(args);
                return Json.Object("platform", "windows", "desktop_available", Native.DesktopAvailable(),
                    "selected_app", selected == null ? null : selected.ToJson(), "selected_window_id", selected == null ? null : selected.SelectedWindowId, "permissions_requested", false,
                    "helper_path", Assembly.GetExecutingAssembly().Location, "helper_pid", Process.GetCurrentProcess().Id,
                    "coordinate_space", Coordinates, "capabilities", Json.Object("ui_automation", true, "window_screenshot", true,
                    "foreground_input", true, "background_input", false, "elevated_apps", false, "password_input", false));
            }
            if (method == "apps") { Arguments.Keys(args); return AppIdentity.Apps(); }
            if (method == "windows") {
                Arguments.Keys(args);
                if (selected == null) throw new ComputerError("APP_NOT_SELECTED", "Select an application first.");
                return selected.Windows();
            }
            if (method == "select_app") {
                selected = null; ClearSnapshot();
                Arguments.Keys(args, "bundle_id", "pid", "window_id");
                selected = AppIdentity.Select(Arguments.String(args, "bundle_id", 256), Arguments.Integer(args, "pid", 1, Int32.MaxValue), args.ContainsKey("window_id") ? (long?)Arguments.WindowId(args, "window_id") : null);
                return Json.Object("selected_app", selected.ToJson(), "selected_window_id", selected.SelectedWindowId,
                    "activated", selected.Window != IntPtr.Zero, "dispatched", Input.DeliveryAttempted, "verified", false,
                    "next", selected.Window == IntPtr.Zero
                        ? "Call computer_windows, then computer_select_app with an exact window_id before observing or sending input."
                        : "Call computer_observe to read the selected window.");
            }
            if (method == "observe") {
                Arguments.Keys(args, "screenshot");
                bool includeImage = Arguments.Boolean(args, "screenshot");
                if (selected == null) throw new ComputerError("APP_NOT_SELECTED", "Select an application first.");
                ClearSnapshot();
                Observation initial = Observation.Read(selected);
                WindowImage image = includeImage ? WindowImage.Capture(selected, initial) : null;
                initial.RefreshWindow(selected);
                Observation verified = initial;
                verified.HasScreenshot = image != null;
                last = verified; snapshotId = Guid.NewGuid().ToString(); observedAt = Stopwatch.GetTimestamp();
                var result = verified.ToJson(selected);
                result["snapshot_id"] = snapshotId; result["selected_app"] = selected.ToJson();
                result["coordinate_space"] = Coordinates; result["expires_in_seconds"] = 60; result["untrusted_content"] = true;
                if (image != null) {
                    result["image"] = image.ToJson();
                    result["screenshot"] = Json.Object("width", image.Width, "height", image.Height,
                        "window_width_points", verified.Frame.Width, "window_height_points", verified.Frame.Height,
                        "window_width_pixels", verified.Frame.Width, "window_height_pixels", verified.Frame.Height);
                }
                return result;
            }
            if (method != "click" && method != "type" && method != "key" && method != "scroll" && method != "drag" && method != "set_value" && method != "action") throw new ComputerError("UNKNOWN_METHOD", "Unknown computer helper operation.");
            string token = Arguments.String(args, "snapshot_id", 256);
            if (selected == null || last == null || token != snapshotId || (Stopwatch.GetTimestamp() - observedAt) / (double)Stopwatch.Frequency > 60) throw new ComputerError("STALE_SNAPSHOT", "Snapshot is missing, consumed, expired, or belongs to another session. Observe again.");
            Observation previous = last;
            ClearSnapshot(); // Consume before validation or dispatch, including failed actions.
            if (method == "click") {
                Arguments.Keys(args, "snapshot_id", "ref", "x", "y", "button", "click_count", "modifiers");
                bool reference = args.ContainsKey("ref");
                if (reference ? args.ContainsKey("x") || args.ContainsKey("y") : !args.ContainsKey("x") || !args.ContainsKey("y")) throw Arguments.Invalid();
            } else if (method == "type") Arguments.Keys(args, "snapshot_id", "text");
            else if (method == "key") Arguments.Keys(args, "snapshot_id", "key");
            else if (method == "scroll") Arguments.Keys(args, "snapshot_id", "x", "y", "delta_y");
            else if (method == "drag") Arguments.Keys(args, "snapshot_id", "from_x", "from_y", "to_x", "to_y", "modifiers");
            else if (method == "set_value") Arguments.Keys(args, "snapshot_id", "ref", "value");
            else Arguments.Keys(args, "snapshot_id", "ref", "action");
            selected.RequireForeground();
            previous.RefreshWindow(selected);
            Input.Dispatch(method, args, previous, selected);
            return Json.Object("dispatched", true, "verified", false, "verification", Json.Object("status", "not_verified", "method", method), "next", "Observe again and verify the result before continuing.");
        }
    }

    internal static class Program {
        private static readonly object OutputLock = new object();
        private static readonly Stream Output = Console.OpenStandardOutput();
        private static readonly UTF8Encoding Utf8 = new UTF8Encoding(false, true);
        private sealed class Deadline { internal object Id; internal int Active = 1; }

        [MTAThread]
        private static int Main(string[] args) {
            if (args.Length == 1 && args[0] == "--self-test") return SelfTest();
            if (args.Length != 0) return 1;
            var lines = new BlockingCollection<string>(8);
            var reader = new Thread(delegate() { ReadInput(lines); });
            reader.IsBackground = true; reader.Start();
            var session = new Session();
            foreach (string line in lines.GetConsumingEnumerable()) {
                object id = null;
                Input.DeliveryAttempted = false;
                try {
                    var request = Json.Serializer.DeserializeObject(line) as Dictionary<string, object>;
                    if (request == null) throw Arguments.Invalid();
                    Arguments.Keys(request, "id", "method", "params");
                    object supplied;
                    if (request.TryGetValue("id", out supplied) && (supplied is int || supplied is long || supplied is string && ((string)supplied).Length <= 128)) id = supplied;
                    else throw Arguments.Invalid();
                    string method = Arguments.String(request, "method", 64);
                    object raw;
                    if (!request.TryGetValue("params", out raw) || !(raw is Dictionary<string, object>)) throw Arguments.Invalid();
                    var deadline = new Deadline { Id = id };
                    using (var timer = new Timer(Timeout, deadline, 10000, System.Threading.Timeout.Infinite)) {
                        try {
                            object result = session.Handle(method, (Dictionary<string, object>)raw);
                            if (Interlocked.Exchange(ref deadline.Active, 0) == 1) Emit(Json.Object("id", id, "result", result));
                        } finally { Interlocked.Exchange(ref deadline.Active, 0); }
                    }
                } catch (ComputerError error) { Error(id, error.Code, error.Message); }
                catch { Error(id, "NATIVE_ERROR", "The Windows operation could not be verified. Observe again before retrying input."); }
            }
            return 0;
        }
        private static void Timeout(object state) {
            var deadline = (Deadline)state;
            if (Interlocked.CompareExchange(ref deadline.Active, 0, 1) != 1) return;
            Input.EmergencyRelease();
            Error(deadline.Id, "NATIVE_TIMEOUT", "The selected application's accessibility or capture API did not respond. This helper is stopping; observe in a new computer session before input.");
            Environment.Exit(2);
        }
        private static void ReadInput(BlockingCollection<string> lines) {
            try {
                using (var input = new BufferedStream(Console.OpenStandardInput()))
                using (var bytes = new MemoryStream()) {
                    while (true) {
                        int value = input.ReadByte();
                        if (value < 0) { Input.EmergencyRelease(); Environment.Exit(0); }
                        if (value == 10) {
                            string line = Utf8.GetString(bytes.ToArray()).TrimEnd('\r'); bytes.SetLength(0);
                            if (!lines.TryAdd(line)) throw Arguments.Invalid();
                        } else {
                            if (bytes.Length >= 64 * 1024) throw Arguments.Invalid();
                            bytes.WriteByte((byte)value);
                        }
                    }
                }
            } catch {
                Error(null, "INVALID_REQUEST", "Input must be bounded UTF-8 JSON lines.");
                Input.EmergencyRelease(); Environment.Exit(1);
            }
        }
        private static void Error(object id, string code, string message) {
            bool uncertain = Input.DeliveryAttempted || code == "INPUT_UNCERTAIN" || code == "NATIVE_TIMEOUT";
            Emit(Json.Object("id", id, "error", Json.Object("code", code, "message", message, "recoverable", true,
                "dispatched", uncertain ? null : (object)false, "next", uncertain
                    ? "Input may have been delivered. Select the intended window and observe before deciding on another action. This operation was not replayed."
                    : "Refresh with computer_observe before another input; reselect the intended window if its identity or foreground changed.")));
        }
        private static void Emit(object value) {
            try {
                byte[] bytes = Utf8.GetBytes(Json.Encode(value) + "\n");
                if (bytes.Length > 6 * 1024 * 1024) bytes = Utf8.GetBytes(Json.Encode(Json.Object("id", null, "error", Json.Object("code", "OUTPUT_LIMIT", "message", "The native response exceeds the output limit."))) + "\n");
                lock (OutputLock) { Output.Write(bytes, 0, bytes.Length); Output.Flush(); }
            } catch { Input.EmergencyRelease(); Environment.Exit(1); }
        }
        private static int SelfTest() {
            try {
                if (IntPtr.Size != 8 || Marshal.SizeOf(typeof(Native.Input)) != 40) throw new Exception("input layout");
                if (Json.Text("A\uD83D\uDE00B", 3) != "A\uD83D\uDE00" || Json.Text("\uD800", 10) != "\uFFFD") throw new Exception("Unicode limits");
                string bundle = AppIdentity.BundleId("Example App", "C:\\Example\\app.exe");
                if (!bundle.StartsWith("win32:Example App:") || bundle.Length != "win32:Example App:".Length + 64 || AppIdentity.ApplicationName(new string('A', 140)).Length != 128) throw new Exception("application identity");
                bool invalidName = false;
                try { AppIdentity.ApplicationName("spoof:app"); } catch (ComputerError) { invalidName = true; }
                if (!invalidName) throw new Exception("application identity delimiter");
                foreach (var bad in new[] { Json.Object("pid", 0), Json.Object("pid", 1.5), Json.Object("pid", "1") }) {
                    bool rejected = false;
                    try { Arguments.Integer(bad, "pid", 1, Int32.MaxValue); } catch (ComputerError) { rejected = true; }
                    if (!rejected) throw new Exception("PID validation");
                }
                var windows = new List<IntPtr> { new IntPtr(11), new IntPtr(22) };
                if (AppIdentity.ChooseWindow(windows, new IntPtr(99), null) != IntPtr.Zero ||
                    AppIdentity.ChooseWindow(new List<IntPtr>(), IntPtr.Zero, null) != IntPtr.Zero ||
                    AppIdentity.ChooseWindow(windows, new IntPtr(22), null) != new IntPtr(22) ||
                    AppIdentity.ChooseWindow(new List<IntPtr> { new IntPtr(11) }, new IntPtr(99), null) != new IntPtr(11) ||
                    AppIdentity.ChooseWindow(windows, new IntPtr(99), 22) != new IntPtr(22)) throw new Exception("explicit and ambiguous window selection");
                bool invalidWindow = false;
                try { AppIdentity.ChooseWindow(windows, IntPtr.Zero, 99); } catch (ComputerError error) { invalidWindow = error.Code == "WINDOW_NOT_FOUND"; }
                if (!invalidWindow) throw new Exception("unknown explicit window rejected");
                ushort key; List<string> modifiers;
                Input.ParseKey("ctrl+shift+a", out key, out modifiers);
                if (key != 65 || modifiers.Count != 2 || modifiers[0] != "control") throw new Exception("key chord parsing");
                foreach (string invalid in new[] { "ctrl+control+a", "alt+tab", "control+alt+delete", "meta+a", "alt+f4", "shift+", "f13" }) {
                    bool rejected = false;
                    try { Input.ParseKey(invalid, out key, out modifiers); } catch (ComputerError) { rejected = true; }
                    if (!rejected) throw new Exception("unsafe key chord");
                }
                if (Arguments.String(Json.Object("value", ""), "value", 2000, true) != "") throw new Exception("empty text value");
                if (Arguments.WindowId(Json.Object("window_id", 1234), "window_id") != 1234) throw new Exception("window identity");
                foreach (var bad in new[] { Json.Object("window_id", 0), Json.Object("window_id", 1.5), Json.Object("window_id", "1234"), Json.Object("window_id", 9007199254740992d) }) {
                    bool rejected = false;
                    try { Arguments.WindowId(bad, "window_id"); } catch (ComputerError) { rejected = true; }
                    if (!rejected) throw new Exception("window identity validation");
                }
                foreach (var bad in new[] { Json.Object("modifiers", "shift"), Json.Object("modifiers", new[] { "shift", "shift" }), Json.Object("modifiers", new[] { "command" }) }) {
                    bool rejected = false;
                    try { Arguments.Modifiers(bad); } catch (ComputerError) { rejected = true; }
                    if (!rejected) throw new Exception("modifier validation");
                }
                var session = new Session();
                foreach (string method in new[] { "click", "type", "key", "scroll", "drag", "set_value", "action" }) {
                    bool rejected = false;
                    try { session.Handle(method, Json.Object("snapshot_id", "unissued")); } catch (ComputerError error) { rejected = error.Code == "STALE_SNAPSHOT"; }
                    if (!rejected) throw new Exception("snapshot validation");
                }
                Emit(Json.Object("self_test", true, "checks", new[] { "native input layout", "Unicode truncation", "readable bound application identity", "strict numeric validation", "unissued snapshots rejected", "key chord parsing", "system chord rejection", "window identity validation", "empty text values", "modifier validation", "explicit and ambiguous window selection" }));
                return 0;
            } catch { Emit(Json.Object("self_test", false)); return 1; }
        }
    }
}

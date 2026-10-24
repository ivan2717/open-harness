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
        private string snapshotId, screenshotDigest;
        private long observedAt;
        internal const string Coordinates = "window-local physical pixels; origin top left";
        private void ClearSnapshot() { last = null; snapshotId = null; screenshotDigest = null; }

        internal object Handle(string method, Dictionary<string, object> args) {
            if (method == "status") {
                Arguments.Keys(args);
                return Json.Object("platform", "windows", "desktop_available", Native.DesktopAvailable(),
                    "selected_app", selected == null ? null : selected.ToJson(), "permissions_requested", false,
                    "helper_path", Assembly.GetExecutingAssembly().Location, "helper_pid", Process.GetCurrentProcess().Id,
                    "coordinate_space", Coordinates, "capabilities", Json.Object("ui_automation", true, "window_screenshot", true,
                    "foreground_input", true, "background_input", false, "elevated_apps", false, "password_input", false));
            }
            if (method == "apps") { Arguments.Keys(args); return AppIdentity.Apps(); }
            if (method == "select_app") {
                selected = null; ClearSnapshot();
                Arguments.Keys(args, "bundle_id", "pid");
                selected = AppIdentity.Select(Arguments.String(args, "bundle_id", 256), Arguments.Integer(args, "pid", 1, Int32.MaxValue));
                return Json.Object("selected_app", selected.ToJson(), "dispatched", true, "verified", false, "next", "Call computer_observe to read the selected window.");
            }
            if (method == "observe") {
                Arguments.Keys(args, "screenshot");
                bool includeImage = Arguments.Boolean(args, "screenshot");
                if (selected == null) throw new ComputerError("APP_NOT_SELECTED", "Select an application first.");
                ClearSnapshot();
                Observation initial = Observation.Read(selected);
                WindowImage image = includeImage ? WindowImage.Capture(selected, initial) : null;
                Observation verified = Observation.Read(selected);
                if (initial.Fingerprint != verified.Fingerprint) throw new ComputerError("STATE_CHANGED", "The window changed during observation. Observe again.");
                last = verified; snapshotId = Guid.NewGuid().ToString(); observedAt = Stopwatch.GetTimestamp();
                screenshotDigest = image == null ? null : image.Digest;
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
            if (method != "click" && method != "type" && method != "key" && method != "scroll") throw new ComputerError("UNKNOWN_METHOD", "Unknown computer helper operation.");
            string token = Arguments.String(args, "snapshot_id", 256);
            if (selected == null || last == null || token != snapshotId || (Stopwatch.GetTimestamp() - observedAt) / (double)Stopwatch.Frequency > 60) throw new ComputerError("STALE_SNAPSHOT", "Snapshot is missing, consumed, expired, or belongs to another session. Observe again.");
            Observation previous = last; string pixels = screenshotDigest;
            ClearSnapshot(); // Consume before validation or dispatch, including failed actions.
            if (method == "click") {
                Arguments.Keys(args, "snapshot_id", "ref", "x", "y");
                bool reference = args.ContainsKey("ref");
                if (reference ? args.ContainsKey("x") || args.ContainsKey("y") : !args.ContainsKey("x") || !args.ContainsKey("y")) throw Arguments.Invalid();
            } else if (method == "type") Arguments.Keys(args, "snapshot_id", "text");
            else if (method == "key") Arguments.Keys(args, "snapshot_id", "key");
            else Arguments.Keys(args, "snapshot_id", "x", "y", "delta_y");
            selected.RequireForeground();
            Observation current = Observation.Read(selected);
            if (current.Fingerprint != previous.Fingerprint || current.FocusRuntime != previous.FocusRuntime) throw new ComputerError("STATE_CHANGED", "The window, focus or accessibility tree changed. No input was dispatched. Observe again.");
            if (method == "scroll" || method == "click" && !args.ContainsKey("ref")) {
                if (pixels == null) throw new ComputerError("SCREENSHOT_REQUIRED", "Coordinate input requires computer_observe with screenshot: true.");
                if (WindowImage.Capture(selected, current).Digest != pixels) throw new ComputerError("STATE_CHANGED", "Window pixels changed after the screenshot. No input was dispatched. Observe again.");
            }
            if (args.ContainsKey("ref")) {
                string reference = Arguments.String(args, "ref", 256); string before, after;
                if (!previous.RuntimeIds.TryGetValue(reference, out before) || !current.RuntimeIds.TryGetValue(reference, out after) || before != after) throw new ComputerError("INVALID_REF", "The referenced element is absent or was replaced. Observe again.");
            }
            Input.Dispatch(method, args, current, selected);
            return Json.Object("dispatched", true, "verified", false, "next", "Observe again and verify the result before continuing.");
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
        private static void Error(object id, string code, string message) { Emit(Json.Object("id", id, "error", Json.Object("code", code, "message", message))); }
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
                foreach (var bad in new[] { Json.Object("pid", 0), Json.Object("pid", 1.5), Json.Object("pid", "1") }) {
                    bool rejected = false;
                    try { Arguments.Integer(bad, "pid", 1, Int32.MaxValue); } catch (ComputerError) { rejected = true; }
                    if (!rejected) throw new Exception("PID validation");
                }
                var session = new Session();
                foreach (string method in new[] { "click", "type", "key", "scroll" }) {
                    bool rejected = false;
                    try { session.Handle(method, Json.Object("snapshot_id", "unissued")); } catch (ComputerError error) { rejected = error.Code == "STALE_SNAPSHOT"; }
                    if (!rejected) throw new Exception("snapshot validation");
                }
                Emit(Json.Object("self_test", true, "checks", new[] { "native input layout", "Unicode truncation", "strict numeric validation", "unissued snapshots rejected" }));
                return 0;
            } catch { Emit(Json.Object("self_test", false)); return 1; }
        }
    }
}

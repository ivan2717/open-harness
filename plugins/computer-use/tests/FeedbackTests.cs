using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Threading;
using System.Windows.Forms;

namespace OpenHarness.ComputerUse {
    internal static class FeedbackTests {
        [DllImport("user32.dll")] private static extern bool GetCursorPos(out Native.Point point);
        private static void Check(bool value, string message) { if (!value) throw new Exception(message); }
        private static void Pump(int milliseconds) {
            var timer = Stopwatch.StartNew();
            while (timer.ElapsedMilliseconds < milliseconds) { Application.DoEvents(); Thread.Sleep(10); }
        }
        private static Dictionary<string, object> State(FeedbackPreview preview) { return (Dictionary<string, object>)preview.State(); }
        private static void RecoveryPolicy() {
            var recovery = new PreviewRecovery(); double now = 0;
            foreach (double delay in new[] { 1.0, 2.0, 4.0, 8.0, 8.0 }) {
                recovery.Fail(now);
                Check(!recovery.Ready(now + delay - 0.01), "Retry respects backoff");
                now += delay; Check(recovery.Ready(now), "Transient failure is retryable");
            }
            recovery.Fail(now); Check(!recovery.Ready(now + 3600), "Persistent failure exhausts automatic retries");
            recovery.Reset(); Check(recovery.Ready(now) && !recovery.Failed, "Explicit observation resets recovery");
        }
        private static void RecoveryLifecycle(AppIdentity identity) {
            double now = 0; int calls = 0; bool succeed = false;
            using (var preview = new FeedbackPreview((window, bounds) => { Interlocked.Increment(ref calls); return succeed ? "/9j/recovered" : null; }, () => now)) {
                var dispatcher = preview.Handle; preview.SetEnabled(true); preview.Observe(identity); Pump(400);
                Check(calls == 1, "Failed capture waits before retrying");
                for (int retry = 0; retry < 5; retry++) { now += 10; Pump(400); }
                Check(calls == 6, "Capture has a bounded retry budget");
                now += 100; Pump(400); Check(calls == 6, "Exhausted capture does not spin");
                succeed = true; preview.Observe(identity); Pump(500);
                Check((int)State(preview)["frames_received"] > 0, "Observe recovers the same target after retry exhaustion");
            }
            now = 0; calls = 0; bool lateFrame = false;
            using (var gate = new ManualResetEvent(false))
            using (var preview = new FeedbackPreview((window, bounds) => {
                if (Interlocked.Increment(ref calls) == 1) { gate.WaitOne(); return "/9j/late"; }
                return "/9j/current";
            }, () => now)) {
                var previousEmit = FeedbackPreview.Emit;
                FeedbackPreview.Emit = value => {
                    var packet = (Dictionary<string, object>)value;
                    object jpeg; if (packet.TryGetValue("jpeg", out jpeg) && (string)jpeg == "/9j/late") lateFrame = true;
                };
                try {
                    var dispatcher = preview.Handle; preview.SetEnabled(true); preview.Observe(identity); Pump(400);
                    now = 3; Pump(350); now = 50; Pump(700);
                    Check(calls == 1, "Timed-out PrintWindow must not spawn concurrent workers");
                    preview.Release(); preview.Observe(identity); Pump(350);
                    Check(calls == 1, "New observation also waits for the old native call");
                    gate.Set(); Pump(700);
                    Check(!lateFrame && (int)State(preview)["frames_received"] > 0, "Old result is discarded and new capture resumes");
                } finally { gate.Set(); FeedbackPreview.Emit = previousEmit; }
            }
        }
        [STAThread] private static int Main() {
            try {
                RecoveryPolicy();
                Check(Native.DesktopAvailable(), "An unlocked interactive Windows desktop is required for feedback acceptance.");
                using (var source = new Form { Text = "Open Harness preview test only", BackColor = Color.Teal, ClientSize = new Size(600, 360) })
                using (var replacement = new Form { Text = "Open Harness second test window", ClientSize = new Size(400, 500) })
                using (var preview = new FeedbackPreview()) {
                    source.Controls.Add(new Label { Text = "Local preview test", AutoSize = true });
                    replacement.Controls.Add(new Label { Text = "Second window", AutoSize = true });
                    source.Show(); var dispatcher = preview.Handle; preview.SetEnabled(true); Pump(150);
                    using (var process = Process.GetCurrentProcess()) {
                        var identity = new AppIdentity { Pid = process.Id, Started = process.StartTime.ToUniversalTime().Ticks,
                            Window = source.Handle, Name = "Test fixture" };
                        RecoveryLifecycle(identity);
                        IntPtr foreground = Native.GetForegroundWindow();
                        preview.Observe(identity); Pump(1500);
                        Check((bool)State(preview)["in_app"] && (int)State(preview)["frames_received"] > 0, "Local capture delivers frames to the in-app view");
                        Check(Native.GetForegroundWindow() == foreground, "Preview must not activate");
                        Check(Application.OpenForms.Count == 1, "Capture must not create a preview Form");
                        Native.Point before, after; Check(GetCursorPos(out before), "Read pointer before feedback");
                        preview.Pointer(140, 90, true);
                        Check(GetCursorPos(out after) && before.X == after.X && before.Y == after.Y, "Feedback must not move the user's pointer");
                        Pump(200);
                        Check(Native.GetForegroundWindow() == foreground, "Cursor must not activate");
                        preview.SetEnabled(false); Pump(350);
                        int pausedFrames = (int)State(preview)["frames_received"];
                        Pump(400); Check(pausedFrames == (int)State(preview)["frames_received"], "Collapse pauses capture");
                        preview.SetEnabled(true); Pump(1000);
                        Check((int)State(preview)["frames_received"] > pausedFrames, "Expand resumes capture");
                        preview.SuspendInput(); Pump(50);
                        Check(!(bool)State(preview)["cursor_visible"], "Display leaves input hit-testing path");
                        preview.ResumeInput(); Pump(50);
                        Check((bool)State(preview)["active"] && Native.GetForegroundWindow() == foreground, "Resume without activation");
                        replacement.Show(); identity.Window = replacement.Handle; preview.Observe(identity); Pump(100);
                        Check((long)State(preview)["window_id"] == replacement.Handle.ToInt64(), "Switch uses exact new HWND");
                        replacement.Close(); Pump(650);
                        Check(!(bool)State(preview)["active"] && !(bool)State(preview)["streaming"], "Closed source releases preview instead of retargeting");
                        identity.Window = source.Handle; preview.Observe(identity); preview.Release(); Pump(50);
                        Check(!(bool)State(preview)["active"] && !(bool)State(preview)["streaming"], "Stop releases display resources");
                    }
                }
                Console.WriteLine("Passed Windows feedback GUI acceptance"); return 0;
            } catch (Exception error) { Console.Error.WriteLine(error); return 1; }
        }
    }
}

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;
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
        private static void PumpUntil(Func<bool> ready, string message) {
            var timer = Stopwatch.StartNew();
            while (!ready() && timer.ElapsedMilliseconds < 10000) { Application.DoEvents(); Thread.Sleep(10); }
            Check(ready(), "Timed out: " + message);
        }
        private static Dictionary<string, object> State(FeedbackPreview preview) { return (Dictionary<string, object>)preview.State(); }
        private static string SourceStatus(AppIdentity identity) {
            string observedStarted;
            try {
                using (var process = Process.GetProcessById(identity.Pid)) observedStarted = process.StartTime.ToUniversalTime().Ticks.ToString();
            } catch (Exception error) { observedStarted = error.GetType().Name; }
            return "desktop=" + Native.DesktopAvailable() + ", window=" + identity.Window +
                ", exists=" + Native.IsWindow(identity.Window) + ", visible=" + Native.IsWindowVisible(identity.Window) +
                ", minimized=" + Native.IsIconic(identity.Window) + ", owner_pid=" + Native.WindowProcess(identity.Window) +
                ", expected_pid=" + identity.Pid + ", started=" + observedStarted + ", expected_started=" + identity.Started;
        }
        private static void ObserveFixture(FeedbackPreview preview, AppIdentity identity) {
            Check(Native.IsWindowVisible(identity.Window), "Fixture must be natively visible: " + SourceStatus(identity));
            preview.Observe(identity);
            Check((bool)State(preview)["active"], "Observation must retain the fixture: " + SourceStatus(identity));
        }
        private static string WindowStatus(IntPtr window) {
            var title = new StringBuilder(256); Native.GetWindowText(window, title, title.Capacity);
            return "hwnd=" + window + ", pid=" + Native.WindowProcess(window) + ", title=" + title;
        }
        private static void CheckStable(FeedbackPreview preview, AppIdentity identity, IntPtr foreground, string phase) {
            Check((bool)State(preview)["active"], phase + ": observation lost; " + SourceStatus(identity));
            IntPtr current = Native.GetForegroundWindow();
            Check(current == foreground, phase + ": foreground changed; expected " + WindowStatus(foreground) +
                "; actual " + WindowStatus(current) + "; cursor_visible=" + State(preview)["cursor_visible"]);
        }
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
            double now = 0; int calls = 0; bool succeed = false; int failures = 0;
            var originalEmit = FeedbackPreview.Emit;
            FeedbackPreview.Emit = value => {
                var packet = (Dictionary<string, object>)value;
                object unavailable; if (packet.TryGetValue("unavailable", out unavailable) && (bool)unavailable) failures++;
            };
            try {
                using (var preview = new FeedbackPreview((window, bounds) => { Interlocked.Increment(ref calls); return succeed ? "/9j/recovered" : null; }, () => now)) {
                    var dispatcher = preview.Handle; preview.SetEnabled(true); ObserveFixture(preview, identity);
                    // Wait for the worker result to reach the UI before advancing
                    // virtual time; a fixed sleep races both timer and pool scheduling.
                    PumpUntil(() => failures == 1, "initial capture failure is processed");
                    Pump(400); Check(calls == 1, "Failed capture waits before retrying");
                    for (int retry = 0; retry < 5; retry++) {
                        now += 10;
                        PumpUntil(() => failures == retry + 2, "retry failure is processed");
                        Check(calls == retry + 2, "Each retry starts exactly one capture");
                    }
                    Check(calls == 6, "Capture has a bounded retry budget");
                    now += 100; Pump(400); Check(calls == 6, "Exhausted capture does not spin");
                    succeed = true; preview.Observe(identity);
                    PumpUntil(() => (int)State(preview)["frames_received"] > 0, "Observe recovers the same target after retry exhaustion");
                }
            } finally { FeedbackPreview.Emit = originalEmit; }
            double blockedNow = 0; int blockedCalls = 0; bool lateFrame = false;
            using (var gate = new ManualResetEvent(false))
            using (var preview = new FeedbackPreview((window, bounds) => {
                if (Interlocked.Increment(ref blockedCalls) == 1) { gate.WaitOne(); return "/9j/late"; }
                return "/9j/current";
            }, () => blockedNow)) {
                var previousEmit = FeedbackPreview.Emit;
                FeedbackPreview.Emit = value => {
                    var packet = (Dictionary<string, object>)value;
                    object jpeg; if (packet.TryGetValue("jpeg", out jpeg) && (string)jpeg == "/9j/late") lateFrame = true;
                };
                try {
                    var dispatcher = preview.Handle; preview.SetEnabled(true); ObserveFixture(preview, identity);
                    PumpUntil(() => Interlocked.CompareExchange(ref blockedCalls, 0, 0) == 1, "blocked capture starts");
                    blockedNow = 3; PumpUntil(() => !(bool)State(preview)["streaming"], "blocked capture times out");
                    blockedNow = 50; Pump(700);
                    Check(blockedCalls == 1, "Timed-out PrintWindow must not spawn concurrent workers");
                    preview.Release(); preview.Observe(identity); Pump(350);
                    Check(blockedCalls == 1, "New observation also waits for the old native call");
                    gate.Set(); PumpUntil(() => (int)State(preview)["frames_received"] > 0, "new capture resumes after old worker exits");
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
                        Check(foreground == identity.Window, "Fixture must be foreground for cursor acceptance: " + WindowStatus(foreground));
                        ObserveFixture(preview, identity);
                        CheckStable(preview, identity, foreground, "Observe synchronously");
                        PumpUntil(() => (int)State(preview)["frames_received"] > 0, "local capture delivers a frame");
                        Check((bool)State(preview)["in_app"] && (int)State(preview)["frames_received"] > 0, "Local capture delivers frames to the in-app view");
                        CheckStable(preview, identity, foreground, "Preview must not activate after capture");
                        Check(Application.OpenForms.Count == 1, "Capture must not create a preview Form");
                        Native.Point before, after; Check(GetCursorPos(out before), "Read pointer before feedback");
                        preview.Pointer(140, 90, true);
                        CheckStable(preview, identity, foreground, "Pointer synchronously");
                        Check((bool)State(preview)["cursor_visible"], "Pointer feedback must be natively visible");
                        Check(GetCursorPos(out after) && before.X == after.X && before.Y == after.Y, "Feedback must not move the user's pointer");
                        Pump(200);
                        CheckStable(preview, identity, foreground, "Pointer after message pump");
                        preview.SetEnabled(false);
                        CheckStable(preview, identity, foreground, "Collapse synchronously");
                        Pump(350); CheckStable(preview, identity, foreground, "Collapse after message pump");
                        int pausedFrames = (int)State(preview)["frames_received"];
                        Pump(400); Check(pausedFrames == (int)State(preview)["frames_received"], "Collapse pauses capture");
                        CheckStable(preview, identity, foreground, "Collapsed observation");
                        preview.SetEnabled(true);
                        CheckStable(preview, identity, foreground, "Expand synchronously");
                        PumpUntil(() => (int)State(preview)["frames_received"] > pausedFrames, "expanded preview delivers a new frame");
                        Check((int)State(preview)["frames_received"] > pausedFrames, "Expand resumes capture");
                        CheckStable(preview, identity, foreground, "Expand after capture");
                        // Refresh feedback so suspension/resumption cannot pass
                        // merely because the earlier pointer has already faded.
                        preview.Pointer(150, 100, false);
                        CheckStable(preview, identity, foreground, "Refresh pointer synchronously");
                        Check((bool)State(preview)["cursor_visible"], "Refreshed pointer must be visible before suspension");
                        preview.SuspendInput();
                        CheckStable(preview, identity, foreground, "Suspend synchronously");
                        Check(!(bool)State(preview)["cursor_visible"], "Display leaves input hit-testing path");
                        Pump(50); CheckStable(preview, identity, foreground, "Suspend after message pump");
                        preview.ResumeInput();
                        CheckStable(preview, identity, foreground, "Resume synchronously");
                        Check((bool)State(preview)["cursor_visible"], "Resume restores fresh pointer feedback");
                        Pump(50); CheckStable(preview, identity, foreground, "Resume after message pump");
                        PumpUntil(() => !(bool)State(preview)["cursor_visible"], "pointer feedback fades and hides");
                        CheckStable(preview, identity, foreground, "Automatic cursor hide");
                        replacement.Show(); identity.Window = replacement.Handle; preview.Observe(identity); Pump(100);
                        Check((long)State(preview)["window_id"] == replacement.Handle.ToInt64(), "Switch uses exact new HWND");
                        replacement.Close(); PumpUntil(() => !(bool)State(preview)["active"], "closed source releases preview");
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

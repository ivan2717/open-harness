using System;
using System.Threading;
using System.Threading.Tasks;
using System.Windows.Forms;

namespace OpenHarness.ComputerUse {
    internal static class VisualFeedback {
        private static readonly object Gate = new object();
        private static readonly ManualResetEvent Ready = new ManualResetEvent(false);
        private static FeedbackPreview preview;
        private static bool started;
        private static bool enabled;
        private static bool Run(Action<FeedbackPreview> action, bool create) {
            lock (Gate) {
                if (!started && create) {
                    started = true;
                    var thread = new Thread(delegate() {
                        try {
                            using (var form = new FeedbackPreview()) {
                                // Force handle creation without activating/showing it.
                                IntPtr handle = form.Handle;
                                preview = form; Ready.Set(); Application.Run();
                            }
                        } catch { preview = null; Ready.Set(); }
                    });
                    thread.IsBackground = true; thread.SetApartmentState(ApartmentState.STA); thread.Start();
                }
            }
            if (!started) return true;
            if (!Ready.WaitOne(2000)) return false;
            var current = preview;
            // An unavailable optional display has no windows to hide. A live
            // but unresponsive display must still fail closed before input.
            if (current == null || current.IsDisposed) return true;
            try {
                var completion = new TaskCompletionSource<bool>();
                current.BeginInvoke((MethodInvoker)delegate {
                    try { action(current); completion.TrySetResult(true); }
                    catch { completion.TrySetResult(false); }
                });
                // No abandoned OS wait handle if a display callback times out.
                return completion.Task.Wait(1000) && completion.Task.Result;
            } catch { return false; }
        }
        internal static void SetEnabled(bool value) { enabled = value; Run(delegate(FeedbackPreview p) { p.SetEnabled(value); }, false); }
        internal static void Observe(AppIdentity identity) { Run(delegate(FeedbackPreview p) { p.SetEnabled(enabled); p.Observe(identity); }, true); }
        internal static void Stop() { Run(delegate(FeedbackPreview p) { p.Release(); }, false); }
        internal static void SuspendInput() {
            if (!Run(delegate(FeedbackPreview p) { p.SuspendInput(); }, false))
                throw new ComputerError("PREVIEW_BUSY", "The local preview could not move out of the input path. No input was dispatched. Observe again.");
        }
        internal static void ResumeInput() { Run(delegate(FeedbackPreview p) { p.ResumeInput(); }, false); }
        internal static void Pointer(int x, int y, bool pressed) { Run(delegate(FeedbackPreview p) { p.Pointer(x, y, pressed); }, false); }
        internal static object State() {
            object result = Json.Object("in_app", true, "active", false, "local_only", true);
            Run(delegate(FeedbackPreview p) { result = p.State(); }, false); return result;
        }
    }
}

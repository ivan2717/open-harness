using System;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.IO;
using System.Threading.Tasks;
using System.Windows.Forms;

namespace OpenHarness.ComputerUse {
    internal sealed class PreviewRecovery {
        internal int Failures { get; private set; }
        internal double? RetryAt { get; private set; }
        internal bool Failed { get { return Failures > 0; } }
        internal void Reset() { Failures = 0; RetryAt = null; }
        internal void Fail(double now) {
            Failures++;
            RetryAt = Failures <= 5 ? (double?)(now + Math.Min(8, Math.Pow(2, Failures - 1))) : null;
        }
        internal bool Ready(double now) { return !Failed || RetryAt.HasValue && now >= RetryAt.Value; }
    }

    // A message-only dispatcher, never a preview Form. The renderer owns the
    // movable view. Capture is bounded to one worker and one selected HWND.
    internal sealed class FeedbackPreview : Control {
        internal static Action<object> Emit = delegate { };
        private readonly Timer monitor = new Timer { Interval = 250 };
        private readonly FeedbackCursor desktopCursor = new FeedbackCursor();
        private IntPtr source;
        private int pid, generation, frames, pointerSequence;
        private long started;
        private string title;
        private bool enabled, suspended, inFlight, captureTimedOut;
        private int captureGeneration;
        private double captureStarted;
        private readonly PreviewRecovery recovery = new PreviewRecovery();
        private readonly Func<IntPtr, Native.Rect, string> capture;
        private readonly Func<double> now;
        private Native.Rect frame;
        private Point? pendingPoint;
        private bool pendingPressed;
        private DateTime pointTime;
        internal FeedbackPreview(Func<IntPtr, Native.Rect, string> capture = null, Func<double> now = null) {
            this.capture = capture ?? new Func<IntPtr, Native.Rect, string>(CaptureFrame);
            this.now = now ?? new Func<double>(() => (double)Stopwatch.GetTimestamp() / Stopwatch.Frequency);
            monitor.Tick += delegate { Tick(); }; monitor.Start();
        }
        protected override CreateParams CreateParams {
            get { var p = base.CreateParams; p.Parent = new IntPtr(-3); return p; } // HWND_MESSAGE
        }
        private void Send(object pointer, string jpeg, bool unavailable) {
            if (source == IntPtr.Zero) { Emit(Json.Object("active", false)); return; }
            var value = Json.Object("active", true, "title", title, "width", frame.Width, "height", frame.Height);
            if (pointer != null) value["pointer"] = pointer;
            if (jpeg != null) value["jpeg"] = jpeg;
            if (unavailable) value["unavailable"] = true;
            Emit(value);
        }
        internal void SetEnabled(bool value) {
            if (enabled == value) return;
            enabled = value; generation++; recovery.Reset();
        }
        internal void Observe(AppIdentity identity) {
            if (source != identity.Window || pid != identity.Pid || started != identity.Started) {
                Release(); source = identity.Window; pid = identity.Pid; started = identity.Started;
                title = Json.Text(identity.Name, 128); frames = 0;
            }
            if (!ValidSource() || !Native.GetWindowRect(source, out frame)) { Release(); return; }
            if (recovery.Failed && !recovery.RetryAt.HasValue) recovery.Reset();
            Send(null, null, false);
        }
        private bool ValidSource() {
            if (!Native.DesktopAvailable() || source == IntPtr.Zero || !Native.IsWindow(source) || Native.WindowProcess(source) != pid || !Native.IsWindowVisible(source) || Native.IsIconic(source)) return false;
            try { using (var process = Process.GetProcessById(pid)) return !process.HasExited && process.StartTime.ToUniversalTime().Ticks == started; }
            catch { return false; }
        }
        private void Tick() {
            if (source == IntPtr.Zero) return;
            if (!ValidSource()) { Release(); return; }
            if (Native.GetForegroundWindow() != source) desktopCursor.Clear();
            if (inFlight) {
                // PrintWindow is not cancellable. Invalidate its eventual frame
                // but retain the single worker until it actually returns.
                if (!captureTimedOut && captureGeneration == generation && now() - captureStarted > 2) {
                    generation++; captureTimedOut = true; recovery.Fail(now()); Send(null, null, true);
                }
                return;
            }
            if (!enabled || suspended || !recovery.Ready(now()) || !Native.GetWindowRect(source, out frame)) return;
            var target = source; var bounds = frame; int revision = generation;
            inFlight = true; captureStarted = now(); captureGeneration = revision; captureTimedOut = false;
            Task.Run(delegate {
                string image = null;
                try { image = capture(target, bounds); } catch { }
                if (IsDisposed) return;
                try { BeginInvoke((MethodInvoker)delegate {
                    inFlight = false;
                    if (revision != generation || !enabled || source != target || !ValidSource()) return;
                    Native.Rect current;
                    if (!Native.GetWindowRect(source, out current) || !bounds.Same(current)) return;
                    if (image == null) { recovery.Fail(now()); Send(null, null, true); }
                    else { recovery.Reset(); frames++; Send(null, image, false); }
                }); } catch { }
            });
        }
        private static string CaptureFrame(IntPtr target, Native.Rect bounds) {
            if (bounds.Width <= 0 || bounds.Height <= 0 || (long)bounds.Width * bounds.Height > 16000000) return null;
            using (var original = new Bitmap(bounds.Width, bounds.Height, PixelFormat.Format32bppRgb)) {
                using (var graphics = Graphics.FromImage(original)) {
                    graphics.Clear(Color.Magenta);
                    IntPtr dc = graphics.GetHdc();
                    try { if (!Native.PrintWindow(target, dc, 0)) return null; }
                    finally { graphics.ReleaseHdc(dc); }
                }
                if (!WindowImage.HasDetail(original)) return null;
                double scale = Math.Min(1.0, 960.0 / Math.Max(bounds.Width, bounds.Height));
                using (var small = new Bitmap(Math.Max(1, (int)(bounds.Width * scale)), Math.Max(1, (int)(bounds.Height * scale)))) {
                    using (var graphics = Graphics.FromImage(small)) { graphics.InterpolationMode = InterpolationMode.HighQualityBilinear; graphics.DrawImage(original, 0, 0, small.Width, small.Height); }
                    using (var bytes = new MemoryStream()) {
                        small.Save(bytes, ImageFormat.Jpeg);
                        return bytes.Length <= 600000 ? Convert.ToBase64String(bytes.ToArray()) : null;
                    }
                }
            }
        }
        internal void SuspendInput() { suspended = true; desktopCursor.Clear(); }
        internal void ResumeInput() { suspended = false; ShowPointer(); }
        internal void Pointer(int x, int y, bool pressed) {
            pendingPoint = new Point(x, y); pendingPressed = pressed; pointTime = DateTime.UtcNow;
            Send(Json.Object("x", x, "y", y, "pressed", pressed, "sequence", ++pointerSequence), null, false);
            if (!suspended) ShowPointer();
        }
        private void ShowPointer() {
            if (!pendingPoint.HasValue || (DateTime.UtcNow - pointTime).TotalSeconds > 1.5 || !ValidSource()) return;
            Native.Rect bounds;
            if (!Native.GetWindowRect(source, out bounds)) return;
            Point point = pendingPoint.Value;
            if (point.X < 0 || point.Y < 0 || point.X >= bounds.Width || point.Y >= bounds.Height) return;
            if (Native.GetForegroundWindow() == source) desktopCursor.Present(new Point(bounds.Left + point.X, bounds.Top + point.Y), pendingPressed);
        }
        internal object State() {
            return Json.Object("in_app", true, "active", source != IntPtr.Zero, "enabled", enabled,
                "window_id", source == IntPtr.Zero ? null : (object)source.ToInt64(), "streaming", source != IntPtr.Zero && enabled && !recovery.Failed,
                "frames_received", frames, "cursor_visible", desktopCursor.Presented, "local_only", true);
        }
        internal void Release() { generation++; desktopCursor.Clear(); source = IntPtr.Zero; pid = 0; started = 0; pendingPoint = null; recovery.Reset(); Send(null, null, false); }
        protected override void Dispose(bool disposing) { if (disposing) { Release(); monitor.Dispose(); desktopCursor.Dispose(); } base.Dispose(disposing); }
    }
}

using System;
using System.ComponentModel;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Windows.Forms;

namespace OpenHarness.ComputerUse {
    // Presentation only: no cursor positioning or input injection calls.
    internal sealed class FeedbackCursor : Form {
        private readonly Timer animation = new Timer { Interval = 16 };
        private DateTime began;
        private Point start, destination;
        private bool pressed;
        internal FeedbackCursor() {
            FormBorderStyle = FormBorderStyle.None; ShowInTaskbar = false;
            Size = new Size(64, 64);
            animation.Tick += delegate {
                double elapsed = (DateTime.UtcNow - began).TotalSeconds;
                if (elapsed >= 1.5) { Clear(); return; }
                double t = Math.Min(1, elapsed / .16), ease = 1 - Math.Pow(1 - t, 3);
                Render(new Point((int)Math.Round(start.X + (destination.X - start.X) * ease),
                    (int)Math.Round(start.Y + (destination.Y - start.Y) * ease)), Math.Max(0, Math.Min(1, (1.5 - elapsed) / .3)));
            };
        }
        internal bool Presented { get { return IsHandleCreated && Native.IsWindowVisible(Handle); } }
        protected override bool ShowWithoutActivation { get { return true; } }
        protected override CreateParams CreateParams {
            get { var p = base.CreateParams; p.ExStyle |= 0x08000000 | 0x00000080 | 0x00000020 | 0x00080000; return p; }
        }
        protected override void WndProc(ref Message message) {
            if (message.Msg == 0x21) { message.Result = new IntPtr(3); return; } // MA_NOACTIVATE
            if (message.Msg == 0x84) { message.Result = new IntPtr(-1); return; } // HTTRANSPARENT
            base.WndProc(ref message);
        }
        internal void Present(Point point, bool isPressed) {
            Native.Rect bounds;
            start = Presented && Native.GetWindowRect(Handle, out bounds) ? new Point(bounds.Left + 25, bounds.Top + 24) : point;
            destination = point; pressed = isPressed; began = DateTime.UtcNow;
            Render(start, 1); SetPresented(true); animation.Start();
        }
        internal void Clear() { animation.Stop(); if (Presented) SetPresented(false); }
        private void SetPresented(bool value) {
            // Form.Show/Hide and the managed TopMost property can assign focus.
            // Keep both visibility transitions in Win32's nonactivating path.
            const uint noSize = 0x0001, noMove = 0x0002, noZOrder = 0x0004,
                noActivate = 0x0010, showWindow = 0x0040, hideWindow = 0x0080, noOwnerZOrder = 0x0200;
            uint flags = noSize | noMove | noActivate | noOwnerZOrder | (value ? showWindow : hideWindow | noZOrder);
            if (!FeedbackNative.SetWindowPos(Handle, value ? new IntPtr(-1) : IntPtr.Zero, 0, 0, 0, 0, flags))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not change feedback cursor visibility without activation.");
        }
        private void Draw(Graphics graphics) {
            graphics.SmoothingMode = SmoothingMode.AntiAlias;
            for (int i = 29; i >= 4; i -= 2) {
                using (var glow = new SolidBrush(Color.FromArgb(5, 80, 80, 80)))
                    graphics.FillEllipse(glow, 29 - i, 29 - i, i * 2, i * 2);
            }
            double progress = Math.Min(1, (DateTime.UtcNow - began).TotalSeconds / .35);
            if (pressed && progress < 1) {
                float radius = (float)(5 + 15 * progress);
                using (var ring = new Pen(Color.FromArgb((int)(82 * (1 - progress)), 140, 140, 140), 1))
                    graphics.DrawEllipse(ring, 25 - radius, 24 - radius, radius * 2, radius * 2);
            }
            Point[] arrow = { new Point(25, 24), new Point(27, 46), new Point(32, 40), new Point(36, 47),
                new Point(39, 45), new Point(35, 38), new Point(43, 36) };
            using (var shadow = new Pen(Color.FromArgb(20, 0, 0, 0), 5) { LineJoin = LineJoin.Round }) graphics.DrawPolygon(shadow, arrow);
            using (var fill = new SolidBrush(Color.FromArgb(230, 122, 122, 122))) graphics.FillPolygon(fill, arrow);
            using (var border = new Pen(Color.FromArgb(242, 255, 255, 255), 1.3f) { LineJoin = LineJoin.Round }) graphics.DrawPolygon(border, arrow);
        }
        private void Render(Point point, double opacity) {
            // Per-pixel alpha avoids the colored fringe of a color-key window.
            using (var image = new Bitmap(64, 64, PixelFormat.Format32bppArgb)) {
                using (var graphics = Graphics.FromImage(image)) { graphics.Clear(Color.Transparent); Draw(graphics); }
                IntPtr dc = FeedbackNative.CreateCompatibleDC(IntPtr.Zero);
                if (dc == IntPtr.Zero) return;
                IntPtr bitmap = IntPtr.Zero, previous = IntPtr.Zero;
                try {
                    bitmap = image.GetHbitmap(Color.FromArgb(0)); previous = FeedbackNative.SelectObject(dc, bitmap);
                    var position = new Native.Point { X = point.X - 25, Y = point.Y - 24 };
                    var origin = new Native.Point(); var size = new FeedbackNative.Size { Width = 64, Height = 64 };
                    var blend = new FeedbackNative.Blend { Alpha = (byte)Math.Round(opacity * 255), Format = 1 };
                    FeedbackNative.UpdateLayeredWindow(Handle, IntPtr.Zero, ref position, ref size, dc, ref origin, 0, ref blend, 2);
                } finally {
                    if (previous != IntPtr.Zero) FeedbackNative.SelectObject(dc, previous);
                    if (bitmap != IntPtr.Zero) FeedbackNative.DeleteObject(bitmap);
                    FeedbackNative.DeleteDC(dc);
                }
            }
        }
        protected override void Dispose(bool disposing) { if (disposing) animation.Dispose(); base.Dispose(disposing); }
    }
}

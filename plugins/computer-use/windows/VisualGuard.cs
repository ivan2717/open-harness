using System;
using System.Drawing;

namespace OpenHarness.ComputerUse {
    // Same target-local comparison as macOS: 21x21 capture pixels, ignoring
    // small rendering noise. This supplements identity/UIA checks, not a proof
    // of semantic safety, and never rejects unrelated window animation.
    internal static class VisualGuard {
        internal static bool Matches(Bitmap before, Bitmap after, double x, double y, int windowWidth, int windowHeight) {
            if (before.Width != after.Width || before.Height != after.Height || windowWidth <= 0 || windowHeight <= 0 ||
                Double.IsNaN(x) || Double.IsInfinity(x) || Double.IsNaN(y) || Double.IsInfinity(y) ||
                x < 0 || y < 0 || x >= windowWidth || y >= windowHeight) return false;
            int px = (int)(x * before.Width / windowWidth), py = (int)(y * before.Height / windowHeight);
            int left = Math.Max(0, px - 10), top = Math.Max(0, py - 10);
            int right = Math.Min(before.Width, px + 11), bottom = Math.Min(before.Height, py + 11);
            int changed = 0;
            for (int row = top; row < bottom; row++) {
                for (int col = left; col < right; col++) {
                    Color a = before.GetPixel(col, row), b = after.GetPixel(col, row);
                    if (Math.Abs(a.R - b.R) > 24 || Math.Abs(a.G - b.G) > 24 || Math.Abs(a.B - b.B) > 24) changed++;
                }
            }
            return changed * 10 <= (right - left) * (bottom - top);
        }
    }
}

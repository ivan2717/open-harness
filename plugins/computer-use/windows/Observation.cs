using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Automation;

namespace OpenHarness.ComputerUse {
    internal sealed class Observation {
        internal Native.Rect Frame;
        internal AutomationElement Root, Focused;
        internal string Fingerprint, FocusRuntime;
        internal readonly Dictionary<string, AutomationElement> Refs = new Dictionary<string, AutomationElement>();
        internal readonly Dictionary<string, string> RuntimeIds = new Dictionary<string, string>();
        internal readonly List<object> Nodes = new List<object>();
        internal bool Truncated;

        internal static string Runtime(AutomationElement element) {
            if (element == null) return "";
            int[] id = element.GetRuntimeId();
            if (id == null || id.Length == 0) throw new ComputerError("UIA_UNAVAILABLE", "The window or element has no stable accessibility identity.");
            return String.Join(".", Array.ConvertAll(id, value => value.ToString(System.Globalization.CultureInfo.InvariantCulture)));
        }
        internal static bool Within(AutomationElement element, AppIdentity identity) {
            var visited = new HashSet<string>();
            for (int i = 0; element != null && i < 32; i++) {
                if (element.Current.ProcessId != identity.Pid) return false;
                string id = Runtime(element);
                if (id == identity.WindowRuntime) return true;
                if (!visited.Add(id)) return false;
                element = TreeWalker.RawViewWalker.GetParent(element);
            }
            return false;
        }
        internal static void RequireSafeElement(AutomationElement element, AppIdentity identity) {
            if (element == null || !Within(element, identity)) throw new ComputerError("TARGET_OUTSIDE_APP", "The target is outside the selected process and window.");
            var current = element.Current;
            if (!current.IsEnabled || current.IsOffscreen) throw new ComputerError("TARGET_UNAVAILABLE", "The target is disabled or not visible.");
            for (int i = 0; element != null && i < 32; i++) {
                if (element.Current.IsPassword) throw new ComputerError("SECURE_FIELD", "Password controls require direct user input.");
                if (Runtime(element) == identity.WindowRuntime) return;
                element = TreeWalker.RawViewWalker.GetParent(element);
            }
            throw new ComputerError("TARGET_OUTSIDE_APP", "Cannot verify the target's window ancestry.");
        }
        internal static Observation Read(AppIdentity identity) {
            identity.Validate();
            var state = new Observation();
            if (!Native.GetWindowRect(identity.Window, out state.Frame) || state.Frame.Width <= 0 || state.Frame.Height <= 0) throw new ComputerError("WINDOW_UNAVAILABLE", "The selected window has no readable bounds.");
            state.Root = AutomationElement.FromHandle(identity.Window);
            AutomationElement focus = AutomationElement.FocusedElement;
            if (focus != null && Within(focus, identity)) state.Focused = focus;
            state.FocusRuntime = state.Focused == null ? "" : Runtime(state.Focused);
            var clock = Stopwatch.StartNew();
            state.Walk(state.Root, 0, identity, new HashSet<string>(), clock);
            state.Fingerprint = Json.Hash(Json.Encode(Json.Object(
                "window", identity.Window.ToInt64(), "frame", Bounds(state.Frame.Left, state.Frame.Top, state.Frame.Width, state.Frame.Height),
                "nodes", state.Nodes, "runtime_ids", state.RuntimeIds, "focus", state.FocusRuntime, "truncated", state.Truncated)));
            identity.Validate();
            Native.Rect finalFrame;
            if (!Native.GetWindowRect(identity.Window, out finalFrame) || !state.Frame.Same(finalFrame)) throw new ComputerError("STATE_CHANGED", "The selected window moved during observation. Observe again.");
            return state;
        }
        private void Walk(AutomationElement element, int depth, AppIdentity identity, HashSet<string> seen, Stopwatch clock) {
            if (Nodes.Count >= 250 || depth >= 16 || clock.ElapsedMilliseconds > 5000) { Truncated = true; return; }
            var info = element.Current;
            if (info.ProcessId != identity.Pid) { Truncated = true; return; }
            string runtime = Runtime(element);
            if (!seen.Add(runtime)) return;
            string reference = "e" + Nodes.Count;
            var node = Json.Object("ref", reference, "role", info.ControlType.ProgrammaticName.Replace("ControlType.", ""), "depth", depth,
                "name", info.IsPassword ? "Password" : Json.Text(info.Name, 400), "automation_id", Json.Text(info.AutomationId, 200),
                "enabled", info.IsEnabled, "focused", info.HasKeyboardFocus, "is_password", info.IsPassword);
            var box = info.BoundingRectangle;
            if (!box.IsEmpty && Finite(box.X) && Finite(box.Y) && Finite(box.Width) && Finite(box.Height)) node["bounds"] = Bounds(box.X - Frame.Left, box.Y - Frame.Top, box.Width, box.Height);
            if (!info.IsPassword) {
                object pattern;
                if (element.TryGetCurrentPattern(ValuePattern.Pattern, out pattern)) node["value"] = Json.Text(((ValuePattern)pattern).Current.Value, 800);
            }
            Nodes.Add(node); Refs.Add(reference, element); RuntimeIds.Add(reference, runtime);
            AutomationElement child = TreeWalker.RawViewWalker.GetFirstChild(element);
            while (child != null) {
                if (Nodes.Count >= 250 || clock.ElapsedMilliseconds > 5000) { Truncated = true; break; }
                Walk(child, depth + 1, identity, seen, clock);
                child = TreeWalker.RawViewWalker.GetNextSibling(child);
            }
        }
        private static bool Finite(double value) { return !Double.IsNaN(value) && !Double.IsInfinity(value); }
        internal static object Bounds(double x, double y, double width, double height) { return Json.Object("x", x, "y", y, "width", width, "height", height); }
        internal Dictionary<string, object> ToJson(AppIdentity identity) {
            return Json.Object("window", Json.Object("id", identity.Window.ToInt64(), "bounds", Bounds(Frame.Left, Frame.Top, Frame.Width, Frame.Height)), "elements", Nodes, "truncated", Truncated);
        }
    }

    internal sealed class WindowImage {
        internal byte[] Bytes;
        internal int Width, Height;
        internal string Digest { get { return Json.Hash(Convert.ToBase64String(Bytes)); } }
        internal object ToJson() { return Json.Object("mimeType", "image/png", "data", Convert.ToBase64String(Bytes)); }

        internal static WindowImage Capture(AppIdentity identity, Observation state) {
            identity.Validate();
            Native.Rect before;
            if (!Native.GetWindowRect(identity.Window, out before) || !state.Frame.Same(before)) throw new ComputerError("STATE_CHANGED", "The window changed before capture.");
            int width = before.Width, height = before.Height;
            if (width <= 0 || height <= 0 || (long)width * height > 16000000) throw new ComputerError("CAPTURE_UNAVAILABLE", "The selected window exceeds the capture limit.");
            using (var original = new Bitmap(width, height, PixelFormat.Format32bppRgb)) {
                using (Graphics graphics = Graphics.FromImage(original)) {
                    graphics.Clear(Color.Magenta);
                    IntPtr dc = graphics.GetHdc();
                    bool captured;
                    try { captured = Native.PrintWindow(identity.Window, dc, 0); }
                    finally { graphics.ReleaseHdc(dc); }
                    if (!captured) throw new ComputerError("CAPTURE_UNAVAILABLE", "This application does not support window-only capture. No desktop capture fallback is used.");
                }
                // PrintWindow can report success for an unsupported GPU surface.
                // Uniform output is not trustworthy visual evidence for input.
                if (!HasDetail(original)) throw new ComputerError("CAPTURE_UNAVAILABLE", "The selected window returned an empty capture. Use accessibility references or a supported application.");
                double scale = Math.Min(1.0, 1600.0 / Math.Max(width, height));
                int targetWidth = Math.Max(1, (int)Math.Round(width * scale));
                int targetHeight = Math.Max(1, (int)Math.Round(height * scale));
                using (var resized = new Bitmap(targetWidth, targetHeight, PixelFormat.Format32bppRgb)) {
                    using (Graphics graphics = Graphics.FromImage(resized)) {
                        graphics.InterpolationMode = InterpolationMode.HighQualityBicubic;
                        graphics.DrawImage(original, new Rectangle(0, 0, targetWidth, targetHeight));
                    }
                    using (var stream = new MemoryStream()) {
                        resized.Save(stream, ImageFormat.Png);
                        if (stream.Length > 4 * 1024 * 1024) throw new ComputerError("CAPTURE_UNAVAILABLE", "The PNG exceeds the image limit.");
                        identity.Validate(); Native.Rect after;
                        if (!Native.GetWindowRect(identity.Window, out after) || !before.Same(after)) throw new ComputerError("STATE_CHANGED", "The selected window changed during capture.");
                        return new WindowImage { Bytes = stream.ToArray(), Width = targetWidth, Height = targetHeight };
                    }
                }
            }
        }
        private static bool HasDetail(Bitmap bitmap) {
            int first = bitmap.GetPixel(0, 0).ToArgb();
            for (int y = 0; y < bitmap.Height; y += Math.Max(1, bitmap.Height / 48))
                for (int x = 0; x < bitmap.Width; x += Math.Max(1, bitmap.Width / 48))
                    if (bitmap.GetPixel(x, y).ToArgb() != first) return true;
            return false;
        }
    }
}

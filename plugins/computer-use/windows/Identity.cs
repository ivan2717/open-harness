using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Text;
using System.Threading;
using System.Windows.Automation;

namespace OpenHarness.ComputerUse {
    internal sealed class AppIdentity {
        internal int Pid, Session;
        internal long Started;
        internal string Path, Bundle, Name;
        internal IntPtr Window;
        internal uint WindowThread;
        internal string WindowRuntime;

        internal static string ApplicationName(string processName) {
            string name = Json.Text(processName, 128);
            if (name.Length == 0 || name.IndexOf(':') >= 0) throw new ComputerError("APP_UNAVAILABLE", "Cannot represent this application's identity safely.");
            return name;
        }
        internal static string BundleId(string name, string canonicalPath) {
            return "win32:" + ApplicationName(name) + ":" + Json.Hash(canonicalPath.ToUpperInvariant());
        }

        internal static AppIdentity Read(int pid) {
            IntPtr process = Native.OpenProcess(0x1000, false, pid);
            if (process == IntPtr.Zero) throw new ComputerError("APP_NOT_FOUND", "The selected application is unavailable.");
            try {
                var path = new StringBuilder(32768); uint size = 32768;
                if (!Native.QueryFullProcessImageName(process, 0, path, ref size)) throw new ComputerError("APP_UNAVAILABLE", "Cannot verify the selected executable.");
                string canonical;
                using (var file = Native.CreateFile(path.ToString(), 0x80, 7, IntPtr.Zero, 3, 0, IntPtr.Zero)) {
                    if (file.IsInvalid) throw new ComputerError("APP_UNAVAILABLE", "Cannot verify the selected executable.");
                    var resolved = new StringBuilder(32768);
                    uint length = Native.GetFinalPathNameByHandle(file, resolved, 32768, 0);
                    if (length == 0 || length >= 32768) throw new ComputerError("APP_UNAVAILABLE", "Cannot resolve the selected executable.");
                    canonical = resolved.ToString();
                }
                int integrity = Native.Integrity(process);
                if (integrity > 0x2000 || integrity > Native.Integrity(Native.GetCurrentProcess())) throw new ComputerError("ELEVATED_APP", "Elevated applications and UAC prompts require direct user interaction. No elevation is requested.");
                using (Process target = Process.GetProcessById(pid)) {
                    if (target.HasExited || target.SessionId != Process.GetCurrentProcess().SessionId) throw new ComputerError("APP_UNAVAILABLE", "The application is outside this interactive session.");
                    string name = ApplicationName(target.ProcessName);
                    return new AppIdentity { Pid = pid, Started = target.StartTime.ToUniversalTime().Ticks, Session = target.SessionId, Path = canonical,
                        Bundle = BundleId(name, canonical), Name = name };
                }
            } finally { Native.CloseHandle(process); }
        }
        internal object SelectedWindowId { get { return Window == IntPtr.Zero ? null : (object)Window.ToInt64(); } }
        internal object ToJson() { return Json.Object("pid", Pid, "bundle_id", Bundle); }
        internal void ValidateProcess() {
            Native.RequireDesktop();
            AppIdentity now = Read(Pid);
            if (now.Started != Started || now.Session != Session || !String.Equals(now.Path, Path, StringComparison.Ordinal)) throw new ComputerError("APP_CHANGED", "The selected process identity changed. Select the application again.");
        }
        internal void Validate() {
            ValidateProcess();
            if (Window == IntPtr.Zero) throw new ComputerError("WINDOW_NOT_SELECTED", "The app is selected but no window is bound. List computer_windows and select_app with an exact window_id.");
            uint pid;
            if (!Native.IsWindow(Window) || Native.GetWindowThreadProcessId(Window, out pid) != WindowThread || pid != Pid || !Native.IsWindowVisible(Window) || Native.IsIconic(Window)) throw new ComputerError("WINDOW_CHANGED", "The selected window closed, changed, or is minimized. Select the intended window again.");
            var root = AutomationElement.FromHandle(Window);
            if (root == null || root.Current.ProcessId != Pid || Observation.Runtime(root) != WindowRuntime) throw new ComputerError("WINDOW_CHANGED", "The selected window's accessibility identity changed. Select it again.");
        }
        internal void RequireForeground() {
            Validate();
            if (Native.GetForegroundWindow() != Window) throw new ComputerError("FOCUS_CHANGED", "The selected window is no longer foreground. No input was dispatched. Select the application and observe again.");
            if (!Native.IsWindowEnabled(Window)) throw new ComputerError("WINDOW_BLOCKED", "The selected window is blocked by a dialog. Select the intended window explicitly.");
        }
        internal static AppIdentity Select(string bundle, int pid, long? windowId) {
            Native.RequireDesktop();
            AppIdentity identity = Read(pid);
            if (!String.Equals(bundle, identity.Bundle, StringComparison.Ordinal)) throw new ComputerError("APP_NOT_FOUND", "Exact application identity and PID must match a listed running application.");
            var candidates = new List<IntPtr>();
            IntPtr foreground = Native.GetForegroundWindow();
            Native.EnumWindows(delegate(IntPtr window, IntPtr ignored) {
                if (Native.WindowProcess(window) == pid && Native.IsWindowVisible(window) && Native.GetAncestor(window, 2) == window) {
                    Native.Rect rectangle;
                    if (Native.GetWindowRect(window, out rectangle) && rectangle.Width > 0 && rectangle.Height > 0) candidates.Add(window);
                }
                return true;
            }, IntPtr.Zero);
            IntPtr chosen = ChooseWindow(candidates, foreground, windowId);
            // Preserve the approved app selection when a window needs choosing.
            // Returning an error here would make the host discard a fresh grant
            // and terminate this helper before computer_windows can enumerate it.
            if (chosen == IntPtr.Zero) return identity;
            identity.Window = chosen; uint owner;
            identity.WindowThread = Native.GetWindowThreadProcessId(chosen, out owner);
            var element = AutomationElement.FromHandle(chosen);
            identity.WindowRuntime = Observation.Runtime(element);
            Input.IdleInput();
            if (Native.IsIconic(chosen)) { Input.DeliveryAttempted = true; Native.ShowWindowAsync(chosen, 9); }
            for (int i = 0; i < 20 && Native.IsIconic(chosen); i++) Thread.Sleep(25);
            Foreground.Activate(identity);
            identity.RequireForeground();
            return identity;
        }
        internal static IntPtr ChooseWindow(List<IntPtr> candidates, IntPtr foreground, long? requested) {
            if (requested.HasValue) {
                foreach (IntPtr candidate in candidates) if (candidate.ToInt64() == requested.Value) return candidate;
                throw new ComputerError("WINDOW_NOT_FOUND", "The explicit window_id is not a current window of the selected application.");
            }
            if (candidates.Contains(foreground)) return foreground;
            return candidates.Count == 1 ? candidates[0] : IntPtr.Zero;
        }
        internal object Windows() {
            ValidateProcess();
            var windows = new List<object>();
            Native.EnumWindows(delegate(IntPtr window, IntPtr ignored) {
                if (windows.Count >= 128) return false;
                if (Native.WindowProcess(window) != Pid || !Native.IsWindowVisible(window) || Native.GetAncestor(window, 2) != window) return true;
                Native.Rect bounds;
                if (!Native.GetWindowRect(window, out bounds) || bounds.Width <= 0 || bounds.Height <= 0) return true;
                var title = new StringBuilder(1024);
                Native.GetWindowText(window, title, title.Capacity);
                windows.Add(Json.Object("id", window.ToInt64(), "title", Json.Text(title.ToString(), 400),
                    "bounds", Observation.Bounds(bounds.Left, bounds.Top, bounds.Width, bounds.Height),
                    "selected", window == Window, "minimized", Native.IsIconic(window), "foreground", window == Native.GetForegroundWindow()));
                return true;
            }, IntPtr.Zero);
            ValidateProcess();
            return Json.Object("selected_app", ToJson(), "selected_window_id", SelectedWindowId, "windows", windows, "untrusted_content", true);
        }
        internal static object Apps() {
            Native.RequireDesktop();
            var applications = new List<object>(); var seen = new HashSet<int>();
            Native.EnumWindows(delegate(IntPtr window, IntPtr ignored) {
                if (applications.Count >= 128) return false;
                if (!Native.IsWindowVisible(window) || Native.GetAncestor(window, 2) != window) return true;
                int pid = (int)Native.WindowProcess(window);
                if (pid <= 0 || !seen.Add(pid)) return true;
                try {
                    var identity = Read(pid);
                    applications.Add(Json.Object("name", identity.Name, "bundle_id", identity.Bundle, "pid", pid));
                } catch { /* Inaccessible/elevated/system processes are never candidates. */ }
                return true;
            }, IntPtr.Zero);
            return Json.Object("applications", applications);
        }
    }
}

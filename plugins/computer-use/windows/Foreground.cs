using System;
using System.Runtime.InteropServices;
using System.Threading;
using System.Windows.Automation;

namespace OpenHarness.ComputerUse {
    internal static class Foreground {
        private static ComputerError Refused() {
            return new ComputerError("FOCUS_CHANGED", "Windows did not activate the selected window. Bring that application to the foreground once, then select and observe it again. No keyboard or mouse input was dispatched.");
        }
        private static void RequireOriginal(IntPtr original, AppIdentity identity) {
            IntPtr current = Native.GetForegroundWindow();
            if (current != original && current != identity.Window) throw new ComputerError("FOCUS_CHANGED", "The foreground window changed during application selection. Select the intended application again.");
        }
        private static bool Wait(AppIdentity identity, IntPtr original) {
            // Activation can complete asynchronously across input queues. Never
            // keep reclaiming focus after the user switches to another window.
            for (int attempt = 0; attempt < 10; attempt++) {
                if (Native.GetForegroundWindow() == identity.Window) return true;
                RequireOriginal(original, identity);
                Thread.Sleep(20);
            }
            return Native.GetForegroundWindow() == identity.Window;
        }
        private static void Ready(AppIdentity identity, IntPtr original) {
            identity.Validate();
            if (!Native.IsWindowEnabled(identity.Window)) throw new ComputerError("WINDOW_BLOCKED", "The selected window is blocked by a dialog. Select the intended window explicitly.");
            RequireOriginal(original, identity);
            Input.IdleInput();
        }
        internal static void Activate(AppIdentity identity) {
            IntPtr original = Native.GetForegroundWindow();
            Ready(identity, original);
            if (original == identity.Window) return;
            Native.SetForegroundWindow(identity.Window);
            if (Wait(identity, original)) return;

            // UIA providers can perform their own supported activation. This
            // cross-process call must happen before any input queues are joined.
            Ready(identity, original);
            try { AutomationElement.FromHandle(identity.Window).SetFocus(); }
            catch (ElementNotAvailableException) { /* The next identity check rejects replaced windows. */ }
            catch (InvalidOperationException) { /* Some top-level providers cannot accept focus. */ }
            catch (COMException) { /* Native activation below does not depend on provider support. */ }
            if (Wait(identity, original)) return;

            Ready(identity, original);
            Handoff(identity, original);
            if (!Wait(identity, original)) throw Refused();
        }
        private static void Handoff(AppIdentity identity, IntPtr original) {
            uint owner;
            uint foregroundThread = Native.GetWindowThreadProcessId(original, out owner);
            uint currentThread = Native.GetCurrentThreadId();
            if (original == IntPtr.Zero || foregroundThread == 0 || foregroundThread == currentThread || owner == 0) throw Refused();
            // Only join a normal application in this interactive session. We
            // inspect its process identity, never its controls or screen content.
            try { AppIdentity.Read(checked((int)owner)); }
            catch (ComputerError) { throw Refused(); }
            Ready(identity, original);
            uint currentOwner;
            if (Native.GetWindowThreadProcessId(original, out currentOwner) != foregroundThread || currentOwner != owner) throw Refused();
            // The MTA protocol thread does not pump a window. PM_NOREMOVE
            // explicitly creates its message queue without consuming a message.
            Native.Message message;
            Native.PeekMessage(out message, IntPtr.Zero, 0, 0, 0);
            if (!Native.AttachThreadInput(currentThread, foregroundThread, true)) throw Refused();
            try {
                // While attached use only short local Win32 checks/activation:
                // no UIA, message waits, SetFocus, or synthetic keyboard input.
                Native.RequireDesktop();
                Input.IdleInput();
                RequireOriginal(original, identity);
                uint targetOwner;
                if (Native.GetWindowThreadProcessId(identity.Window, out targetOwner) != identity.WindowThread || targetOwner != identity.Pid ||
                    !Native.IsWindowVisible(identity.Window) || Native.IsIconic(identity.Window) || !Native.IsWindowEnabled(identity.Window)) throw Refused();
                Native.SetForegroundWindow(identity.Window);
            } finally {
                if (!Native.AttachThreadInput(currentThread, foregroundThread, false)) {
                    // Do not service another request with an uncertain input
                    // queue association. Process exit always removes it.
                    Console.Error.WriteLine("Unable to release the foreground input queue; stopping the Computer Use helper.");
                    Environment.Exit(2);
                }
            }
        }
    }
}

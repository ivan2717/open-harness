using System;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace OpenHarness.ComputerUse {
    internal static class Native {
        internal delegate bool EnumWindowCallback(IntPtr window, IntPtr parameter);
        [StructLayout(LayoutKind.Sequential)] internal struct Rect {
            internal int Left, Top, Right, Bottom;
            internal int Width { get { return Right - Left; } }
            internal int Height { get { return Bottom - Top; } }
            internal bool Same(Rect other) { return Left == other.Left && Top == other.Top && Right == other.Right && Bottom == other.Bottom; }
        }
        [StructLayout(LayoutKind.Sequential)] internal struct Point {
            internal int X, Y;
            internal Point(int x, int y) { X = x; Y = y; }
        }
        [StructLayout(LayoutKind.Sequential)] internal struct Message {
            internal IntPtr Window;
            internal uint Id;
            internal UIntPtr WParam;
            internal IntPtr LParam;
            internal uint Time;
            internal Point Position;
            internal uint Private;
        }
        [StructLayout(LayoutKind.Sequential)] internal struct MouseInput {
            internal int X, Y;
            internal uint Data, Flags, Time;
            internal UIntPtr Extra;
        }
        [StructLayout(LayoutKind.Sequential)] internal struct KeyInput {
            internal ushort Key, Scan;
            internal uint Flags, Time;
            internal UIntPtr Extra;
        }
        [StructLayout(LayoutKind.Explicit)] internal struct InputUnion {
            [FieldOffset(0)] internal MouseInput Mouse;
            [FieldOffset(0)] internal KeyInput Key;
        }
        [StructLayout(LayoutKind.Sequential)] internal struct Input {
            internal uint Type;
            internal InputUnion Data;
        }

        [DllImport("user32.dll")] internal static extern bool EnumWindows(EnumWindowCallback callback, IntPtr parameter);
        [DllImport("user32.dll")] internal static extern bool IsWindow(IntPtr window);
        [DllImport("user32.dll")] internal static extern bool IsWindowVisible(IntPtr window);
        [DllImport("user32.dll")] internal static extern bool IsIconic(IntPtr window);
        [DllImport("user32.dll")] internal static extern bool IsWindowEnabled(IntPtr window);
        [DllImport("user32.dll")] internal static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] internal static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
        [DllImport("user32.dll")] internal static extern IntPtr GetAncestor(IntPtr window, uint flags);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] internal static extern int GetWindowText(IntPtr window, StringBuilder text, int maximum);
        [DllImport("user32.dll")] internal static extern bool GetWindowRect(IntPtr window, out Rect rectangle);
        [DllImport("user32.dll")] internal static extern bool SetForegroundWindow(IntPtr window);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] internal static extern bool PeekMessage(out Message message, IntPtr window, uint minimum, uint maximum, uint flags);
        [DllImport("user32.dll", SetLastError = true)] internal static extern bool AttachThreadInput(uint thread, uint otherThread, bool attach);
        [DllImport("user32.dll")] internal static extern bool ShowWindowAsync(IntPtr window, int command);
        [DllImport("user32.dll")] internal static extern IntPtr WindowFromPoint(Point point);
        [DllImport("user32.dll")] internal static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)] internal static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr wparam, IntPtr lparam, uint flags, uint timeout, out UIntPtr result);
        [DllImport("user32.dll")] internal static extern uint SendInput(uint count, Input[] inputs, int size);
        [DllImport("user32.dll")] internal static extern short GetAsyncKeyState(int key);
        [DllImport("user32.dll")] internal static extern int GetSystemMetrics(int index);
        [DllImport("user32.dll")] internal static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
        [DllImport("user32.dll")] internal static extern bool CloseDesktop(IntPtr desktop);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] internal static extern bool GetUserObjectInformation(IntPtr handle, int index, StringBuilder value, uint length, out uint needed);
        [DllImport("user32.dll")] internal static extern IntPtr GetThreadDesktop(uint thread);
        [DllImport("kernel32.dll")] internal static extern uint GetCurrentThreadId();
        [DllImport("kernel32.dll")] internal static extern IntPtr GetCurrentProcess();
        [DllImport("kernel32.dll")] internal static extern IntPtr OpenProcess(uint access, bool inherit, int process);
        [DllImport("kernel32.dll")] internal static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] internal static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder value, ref uint size);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] internal static extern SafeFileHandle CreateFile(string path, uint access, uint sharing, IntPtr security, uint creation, uint flags, IntPtr template);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] internal static extern uint GetFinalPathNameByHandle(SafeFileHandle file, StringBuilder path, uint size, uint flags);
        [DllImport("advapi32.dll")] internal static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
        [DllImport("advapi32.dll")] internal static extern bool GetTokenInformation(IntPtr token, int information, IntPtr buffer, int length, out int required);
        [DllImport("advapi32.dll")] internal static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);
        [DllImport("advapi32.dll")] internal static extern IntPtr GetSidSubAuthority(IntPtr sid, uint index);
        [DllImport("user32.dll")] internal static extern IntPtr GetThreadDpiAwarenessContext();
        [DllImport("user32.dll")] internal static extern int GetAwarenessFromDpiAwarenessContext(IntPtr context);

        internal static bool DesktopAvailable() {
            IntPtr desktop = OpenInputDesktop(0, false, 1); // DESKTOP_READOBJECTS
            if (desktop == IntPtr.Zero) return false;
            try {
                return DesktopName(desktop) == "Default" && DesktopName(GetThreadDesktop(GetCurrentThreadId())) == "Default";
            } finally { CloseDesktop(desktop); }
        }
        private static string DesktopName(IntPtr desktop) {
            var name = new StringBuilder(256); uint required;
            return GetUserObjectInformation(desktop, 2, name, 512, out required) ? name.ToString() : "";
        }
        internal static void RequireDesktop() {
            if (!DesktopAvailable()) throw new ComputerError("DESKTOP_UNAVAILABLE", "Unlock the interactive Windows desktop. Secure desktops and UAC prompts cannot be controlled.");
            try {
                if (GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) != 2) throw new ComputerError("DPI_UNAVAILABLE", "Per-monitor DPI awareness is required for safe window coordinates.");
            } catch (EntryPointNotFoundException) { throw new ComputerError("UNSUPPORTED_WINDOWS", "Computer Use requires Windows 10 version 1703 or later."); }
        }
        internal static uint WindowProcess(IntPtr window) { uint pid; GetWindowThreadProcessId(window, out pid); return pid; }
        internal static int Integrity(IntPtr process) {
            IntPtr token;
            if (!OpenProcessToken(process, 8, out token)) throw new ComputerError("APP_UNAVAILABLE", "Cannot verify the application's integrity level.");
            try {
                int size; GetTokenInformation(token, 25, IntPtr.Zero, 0, out size);
                if (size <= 0 || size > 65536) throw new ComputerError("APP_UNAVAILABLE", "Cannot verify the application's integrity level.");
                IntPtr data = Marshal.AllocHGlobal(size);
                try {
                    if (!GetTokenInformation(token, 25, data, size, out size)) throw new ComputerError("APP_UNAVAILABLE", "Cannot verify the application's integrity level.");
                    IntPtr sid = Marshal.ReadIntPtr(data);
                    byte count = Marshal.ReadByte(GetSidSubAuthorityCount(sid));
                    if (count == 0) throw new ComputerError("APP_UNAVAILABLE", "Invalid application integrity identity.");
                    return Marshal.ReadInt32(GetSidSubAuthority(sid, (uint)(count - 1)));
                } finally { Marshal.FreeHGlobal(data); }
            } finally { CloseHandle(token); }
        }
    }
}

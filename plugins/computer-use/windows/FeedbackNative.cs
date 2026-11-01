using System;
using System.Runtime.InteropServices;

namespace OpenHarness.ComputerUse {
    internal static class FeedbackNative {
        [StructLayout(LayoutKind.Sequential)] internal struct Size { internal int Width, Height; }
        [StructLayout(LayoutKind.Sequential, Pack = 1)] internal struct Blend { internal byte Operation, Flags, Alpha, Format; }
        [DllImport("user32.dll")] internal static extern bool UpdateLayeredWindow(IntPtr window, IntPtr destination, ref Native.Point position, ref Size size, IntPtr source, ref Native.Point origin, uint colorKey, ref Blend blend, uint flags);
        [DllImport("gdi32.dll")] internal static extern IntPtr CreateCompatibleDC(IntPtr dc);
        [DllImport("gdi32.dll")] internal static extern bool DeleteDC(IntPtr dc);
        [DllImport("gdi32.dll")] internal static extern IntPtr SelectObject(IntPtr dc, IntPtr item);
        [DllImport("gdi32.dll")] internal static extern bool DeleteObject(IntPtr item);
    }
}

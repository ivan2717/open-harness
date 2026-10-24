using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Web.Script.Serialization;
using System.Windows.Forms;

internal static class Fixture {
    [StructLayout(LayoutKind.Sequential)] private struct IntegrityLabel { internal IntPtr Sid; internal uint Attributes; }
    [DllImport("kernel32.dll")] private static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll")] private static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] private static extern IntPtr LocalFree(IntPtr handle);
    [DllImport("advapi32.dll")] private static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode)] private static extern bool ConvertStringSidToSid(string text, out IntPtr sid);
    [DllImport("advapi32.dll")] private static extern uint GetLengthSid(IntPtr sid);
    [DllImport("advapi32.dll")] private static extern bool SetTokenInformation(IntPtr token, int information, ref IntegrityLabel label, int size);
    [DllImport("advapi32.dll")] private static extern bool GetTokenInformation(IntPtr token, int information, IntPtr buffer, int length, out int required);
    [DllImport("advapi32.dll")] private static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);
    [DllImport("advapi32.dll")] private static extern IntPtr GetSidSubAuthority(IntPtr sid, uint index);

    // Hosted CI may inherit an elevated token. Only this test window lowers its
    // own token before creating UI; production helpers never elevate or change
    // another application's token. The PID remains stable for the test harness.
    private static void MediumIntegrity() {
        IntPtr token;
        if (!OpenProcessToken(GetCurrentProcess(), 0x88, out token)) throw new InvalidOperationException("Cannot open fixture token.");
        try {
            IntPtr sid;
            if (!ConvertStringSidToSid("S-1-16-8192", out sid)) throw new InvalidOperationException("Cannot create fixture integrity SID.");
            try {
                var label = new IntegrityLabel { Sid = sid, Attributes = 0x20 };
                if (!SetTokenInformation(token, 25, ref label, Marshal.SizeOf(typeof(IntegrityLabel)) + (int)GetLengthSid(sid))) throw new InvalidOperationException("Cannot lower fixture integrity.");
            } finally { LocalFree(sid); }
            int size; GetTokenInformation(token, 25, IntPtr.Zero, 0, out size);
            if (size <= 0 || size > 65536) throw new InvalidOperationException("Cannot verify fixture integrity.");
            IntPtr data = Marshal.AllocHGlobal(size);
            try {
                if (!GetTokenInformation(token, 25, data, size, out size)) throw new InvalidOperationException("Cannot verify fixture integrity.");
                IntPtr actual = Marshal.ReadIntPtr(data);
                byte count = Marshal.ReadByte(GetSidSubAuthorityCount(actual));
                if (count == 0 || Marshal.ReadInt32(GetSidSubAuthority(actual, (uint)(count - 1))) != 0x2000) throw new InvalidOperationException("Fixture must run at medium integrity.");
            } finally { Marshal.FreeHGlobal(data); }
        } finally { CloseHandle(token); }
    }
    [STAThread]
    private static int Main(string[] args) {
        string state = null; bool passwordFocus = false; bool mediumIntegrity = false;
        for (int i = 0; i < args.Length; i++) {
            if (args[i] == "--state" && i + 1 < args.Length && Path.IsPathRooted(args[i + 1])) state = args[++i];
            else if (args[i] == "--password-focus" && !passwordFocus) passwordFocus = true;
            else if (args[i] == "--medium-integrity" && !mediumIntegrity) mediumIntegrity = true;
            else return 1;
        }
        try { if (mediumIntegrity) MediumIntegrity(); }
        catch { Console.Error.WriteLine("The fixture could not enter medium integrity."); return 1; }
        Application.EnableVisualStyles(); Application.SetCompatibleTextRenderingDefault(false);
        var form = new Form { Text = "Open Harness Computer Use Fixture", Name = "ComputerUseFixture", ClientSize = new Size(660, 540), StartPosition = FormStartPosition.CenterScreen };
        var input = new TextBox { Name = "TaskInput", AccessibleName = "Task input", Location = new Point(24, 36), Size = new Size(370, 28), TabIndex = 0 };
        var apply = new Button { Name = "ApplyButton", AccessibleName = "Apply", Text = "Apply", Location = new Point(420, 34), Size = new Size(110, 32), TabIndex = 1 };
        var result = new Label { Name = "ResultLabel", Text = "Waiting", Location = new Point(24, 90), AutoSize = true };
        var password = new TextBox { Name = "PasswordInput", AccessibleName = "Password", UseSystemPasswordChar = true, Location = new Point(24, 132), Size = new Size(370, 28), TabIndex = 2 };
        var panel = new Panel { Name = "ScrollPanel", AccessibleName = "Scroll area", Location = new Point(24, 196), Size = new Size(590, 300), AutoScroll = true, TabIndex = 3 };
        for (int i = 0; i < 36; i++) panel.Controls.Add(new Label { Text = "Fixture row " + i, Name = "Row" + i, Location = new Point(12, i * 38), Size = new Size(450, 26) });
        form.Controls.AddRange(new Control[] { input, apply, result, password, panel });
        int clicks = 0; bool ready = false;
        var serializer = new JavaScriptSerializer();
        Action save = delegate {
            if (state == null) return;
            var data = new Dictionary<string, object> { { "text", input.Text }, { "clicks", clicks }, { "scroll_y", -panel.AutoScrollPosition.Y }, { "password_length", password.Text.Length }, { "pid", Process.GetCurrentProcess().Id }, { "ready", ready } };
            string temporary = state + ".pending";
            File.WriteAllText(temporary, serializer.Serialize(data), new UTF8Encoding(false));
            if (File.Exists(state)) File.Replace(temporary, state, null);
            else File.Move(temporary, state);
        };
        apply.Click += delegate { clicks++; result.Text = "Saved: " + input.Text; save(); };
        input.TextChanged += delegate { save(); }; password.TextChanged += delegate { save(); };
        panel.Scroll += delegate { save(); };
        var timer = new Timer { Interval = 100 };
        int scroll = 0;
        timer.Tick += delegate { int current = -panel.AutoScrollPosition.Y; if (current != scroll) { scroll = current; save(); } };
        form.Shown += delegate {
            form.Activate(); if (passwordFocus) password.Focus(); else input.Focus();
            ready = true; save(); timer.Start();
            Console.WriteLine(serializer.Serialize(new Dictionary<string, object> { { "ready", true }, { "pid", Process.GetCurrentProcess().Id } }));
        };
        Application.Run(form); timer.Dispose(); form.Dispose(); return 0;
    }
}

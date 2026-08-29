// 目标窗口枚举辅助（开发探测用）：user32 窗口枚举 P/Invoke。
using System;
using System.Text;
using System.Runtime.InteropServices;

public class WEnum
{
    public delegate bool EnumProc(IntPtr h, IntPtr lp);

    [StructLayout(LayoutKind.Sequential)]
    public struct POINT { public int X; public int Y; }

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int L; public int T; public int R; public int B; }

    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumProc cb, IntPtr lp);

    [DllImport("user32.dll")]
    public static extern bool EnumChildWindows(IntPtr h, EnumProc cb, IntPtr lp);

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr h);

    [DllImport("user32.dll")]
    public static extern bool GetClientRect(IntPtr h, out RECT rect);

    [DllImport("user32.dll")]
    public static extern bool ClientToScreen(IntPtr h, ref POINT point);

    [DllImport("user32.dll")]
    public static extern bool SetProcessDPIAware();

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);

    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern bool GetCursorPos(out POINT p);

    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll")]
    public static extern void mouse_event(uint flags, int dx, int dy, uint data, IntPtr extra);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr h);

    [DllImport("user32.dll")]
    public static extern uint GetCurrentThreadId();

    [DllImport("user32.dll")]
    public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);

    [DllImport("user32.dll")]
    public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);

    [DllImport("user32.dll")]
    public static extern int GetSystemMetrics(int index);

    [StructLayout(LayoutKind.Sequential)]
    public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }

    [StructLayout(LayoutKind.Explicit)]
    public struct INPUT
    {
        [FieldOffset(0)] public uint type;
        [FieldOffset(8)] public MOUSEINPUT mi; // x64：type 后 4 字节对齐填充
    }

    [DllImport("user32.dll", SetLastError = true)]
    public static extern uint SendInput(uint count, INPUT[] inputs, int size);
}

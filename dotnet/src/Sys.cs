// Process-level helpers: environment, home/temp directories, output routing and
// exit handling that mirror Node's `process` / `os` behavior.

using System.Runtime.InteropServices;
using System.Text;

namespace Skills;

internal static partial class Sys
{
    private static readonly object OutLock = new();
    private static volatile bool _redirectStdout;
    private static int _exitCode;
    private static readonly List<Action> ExitHooks = new();

    // ─── Output ───

    /// When set, everything written through <see cref="Out"/> goes to stderr. This
    /// is how `add --json` keeps stdout reserved for the final JSON value.
    public static bool StdoutRedirected
    {
        get => _redirectStdout;
        set => _redirectStdout = value;
    }

    private static readonly Lazy<Stream> StdoutStream = new(Console.OpenStandardOutput);
    private static readonly Lazy<Stream> StderrStream = new(Console.OpenStandardError);

    private static void WriteTo(bool stderr, string s)
    {
        if (s.Length == 0) return;
        lock (OutLock)
        {
            if (OperatingSystem.IsWindows() && WinConsole.TryWrite(stderr, s)) return;
            var stream = stderr ? StderrStream.Value : StdoutStream.Value;
            var bytes = Encoding.UTF8.GetBytes(s);
            try
            {
                stream.Write(bytes, 0, bytes.Length);
                stream.Flush();
            }
            catch (IOException)
            {
                // Broken pipe: ignore, like Node does for EPIPE on exit.
            }
        }
    }

    /// Write to (possibly redirected) stdout.
    public static void Out(string s) => WriteTo(StdoutRedirected, s);

    public static void OutLine(string s = "") => Out(s + "\n");

    /// Write to the real stdout, bypassing redirection.
    public static void RealStdout(string s) => WriteTo(false, s);

    public static void Err(string s) => WriteTo(true, s);

    public static void ErrLine(string s = "") => Err(s + "\n");

    // ─── Exit ───

    public static int ExitCode
    {
        get => Volatile.Read(ref _exitCode);
        set => Volatile.Write(ref _exitCode, value);
    }

    /// Register a hook that runs before the process exits via <see cref="Exit"/>.
    public static void OnExit(Action hook)
    {
        lock (ExitHooks) ExitHooks.Add(hook);
    }

    public static void ClearExitHooks()
    {
        lock (ExitHooks) ExitHooks.Clear();
    }

    /// Equivalent of `process.exit(code)`: runs exit hooks, then terminates
    /// immediately (pending telemetry is intentionally not awaited, as in Node).
    [System.Diagnostics.CodeAnalysis.DoesNotReturn]
    public static void Exit(int code)
    {
        List<Action> hooks;
        lock (ExitHooks)
        {
            hooks = new List<Action>(ExitHooks);
            ExitHooks.Clear();
        }
        foreach (var h in hooks) h();
        Ui.OnProcessExit(code);
        Ui.RestoreTerminal();
        Environment.Exit(code);
        throw new InvalidOperationException("unreachable");
    }

    // ─── Environment ───

    /// `process.env.NAME` when set and non-empty (JS truthiness).
    public static string? Env(string name)
    {
        var v = Environment.GetEnvironmentVariable(name);
        return string.IsNullOrEmpty(v) ? null : v;
    }

    /// `process.env.NAME`, even if empty.
    public static string? EnvRaw(string name) => Environment.GetEnvironmentVariable(name);

    public static bool EnvTruthy(string name) => Env(name) != null;

    /// `process.env.NAME?.trim() || fallback`
    public static string? EnvTrimmed(string name)
    {
        var v = EnvRaw(name)?.Trim();
        return string.IsNullOrEmpty(v) ? null : v;
    }

    public static string Cwd() => Directory.GetCurrentDirectory();

    /// `os.homedir()` (libuv `uv_os_homedir`).
    public static string HomeDir()
    {
        if (OperatingSystem.IsWindows())
        {
            var p = Env("USERPROFILE");
            if (p != null) return p;
            var d = Env("HOMEDRIVE");
            var hp = Env("HOMEPATH");
            if (d != null && hp != null) return d + hp;
            return Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        }
        return Env("HOME") ?? Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
    }

    /// `os.tmpdir()`
    public static string TmpDir()
    {
        if (OperatingSystem.IsWindows())
        {
            var p = Env("TEMP") ?? Env("TMP") ?? ((Env("SystemRoot") ?? Env("windir") ?? "C:\\Windows") + "\\temp");
            if (p.Length > 1 && p.EndsWith('\\') && !p.EndsWith(":\\")) p = p[..^1];
            return p;
        }
        var t = Env("TMPDIR") ?? Env("TMP") ?? Env("TEMP") ?? "/tmp";
        if (t.Length > 1 && t.EndsWith('/')) t = t[..^1];
        return t;
    }

    private const string TempChars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

    /// `fs.mkdtemp(join(tmpdir(), prefix))`
    public static string MkdTemp(string prefix)
    {
        var baseDir = TmpDir();
        Directory.CreateDirectory(baseDir);
        for (var attempt = 0; attempt < 100; attempt++)
        {
            var suffix = new char[6];
            for (var i = 0; i < 6; i++) suffix[i] = TempChars[Random.Shared.Next(TempChars.Length)];
            var dir = NodePath.Join(baseDir, prefix + new string(suffix));
            if (Directory.Exists(dir) || File.Exists(dir)) continue;
            Directory.CreateDirectory(dir);
            return dir;
        }
        throw new IOException("failed to create temp dir");
    }

    public static bool StdinIsTty() => !Console.IsInputRedirected;

    public static bool StdoutIsTty() => !Console.IsOutputRedirected;

    public static int? TerminalColumns()
    {
        if (!StdoutIsTty()) return null;
        try
        {
            var w = Console.WindowWidth;
            return w > 0 ? w : null;
        }
        catch
        {
            return null;
        }
    }

    public static int? TerminalRows()
    {
        if (!StdoutIsTty()) return null;
        try
        {
            var h = Console.WindowHeight;
            return h > 0 ? h : null;
        }
        catch
        {
            return null;
        }
    }

    /// `new Date().toISOString()`
    public static string NowIso() => DateTime.UtcNow.ToString("yyyy-MM-dd'T'HH:mm:ss.fff'Z'", System.Globalization.CultureInfo.InvariantCulture);

    /// Enable ANSI escape processing on the Windows console (Node's libuv
    /// translates escapes itself, so output must render the same way).
    public static void EnableVirtualTerminal()
    {
        if (OperatingSystem.IsWindows()) WinConsole.EnableVt();
    }

    private static partial class WinConsole
    {
        private const int StdOutputHandle = -11;
        private const int StdErrorHandle = -12;
        private const uint EnableVirtualTerminalProcessing = 0x0004;

        [LibraryImport("kernel32.dll", SetLastError = true)]
        private static partial IntPtr GetStdHandle(int nStdHandle);

        [LibraryImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static partial bool GetConsoleMode(IntPtr hConsoleHandle, out uint lpMode);

        [LibraryImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static partial bool SetConsoleMode(IntPtr hConsoleHandle, uint dwMode);

        [LibraryImport("kernel32.dll", SetLastError = true, StringMarshalling = StringMarshalling.Utf16)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static partial bool WriteConsoleW(IntPtr hConsoleOutput, string lpBuffer, uint nNumberOfCharsToWrite, out uint lpNumberOfCharsWritten, IntPtr lpReserved);

        private static readonly Lazy<IntPtr?> OutHandle = new(() => ConsoleHandle(StdOutputHandle));
        private static readonly Lazy<IntPtr?> ErrHandle = new(() => ConsoleHandle(StdErrorHandle));

        private static IntPtr? ConsoleHandle(int std)
        {
            var h = GetStdHandle(std);
            if (h == IntPtr.Zero || h == new IntPtr(-1)) return null;
            return GetConsoleMode(h, out _) ? h : null;
        }

        /// Write UTF-16 text directly to a console handle; false when the stream
        /// is not a console (pipe or file).
        public static bool TryWrite(bool stderr, string s)
        {
            var h = stderr ? ErrHandle.Value : OutHandle.Value;
            if (h == null) return false;
            var offset = 0;
            while (offset < s.Length)
            {
                var chunk = s.Substring(offset, Math.Min(8192, s.Length - offset));
                if (!WriteConsoleW(h.Value, chunk, (uint)chunk.Length, out var written, IntPtr.Zero) || written == 0) return true;
                offset += (int)written;
            }
            return true;
        }

        public static void EnableVt()
        {
            foreach (var h in new[] { OutHandle.Value, ErrHandle.Value })
            {
                if (h is { } handle && GetConsoleMode(handle, out var mode))
                    SetConsoleMode(handle, mode | EnableVirtualTerminalProcessing);
            }
        }
    }
}

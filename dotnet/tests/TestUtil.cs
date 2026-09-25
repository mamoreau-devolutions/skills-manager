namespace Skills.Tests;

/// A temp directory removed on dispose.
internal sealed class TempDir : IDisposable
{
    public string Path { get; } = Sys.MkdTemp("skills-test-");

    public string Join(params string[] parts) => NodePath.Join([Path, .. parts]);

    public void Write(string rel, string contents)
    {
        var full = Join(rel);
        Directory.CreateDirectory(NodePath.Dirname(full));
        File.WriteAllText(full, contents);
    }

    public void Dispose()
    {
        try
        {
            Fs.RemoveAll(Path);
        }
        catch
        {
            // best effort
        }
    }
}

internal static class TestUtil
{
    /// The port's root (the directory holding Skills.sln).
    public static string PortRoot()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir != null && !File.Exists(System.IO.Path.Combine(dir.FullName, "Skills.sln")))
            dir = dir.Parent;
        return dir?.FullName ?? throw new InvalidOperationException("Skills.sln not found above the test output directory");
    }

    /// Build a minimal stored (uncompressed) zip.
    public static byte[] BuildZip(params (string Name, byte[] Data)[] entries)
    {
        using var output = new MemoryStream();
        using var central = new MemoryStream();
        var w = new BinaryWriter(output);
        var c = new BinaryWriter(central);
        foreach (var (name, data) in entries)
        {
            var offset = (uint)output.Length;
            var crc = Crc32.Compute(data);
            var nameBytes = System.Text.Encoding.UTF8.GetBytes(name);
            w.Write(0x04034b50u);
            w.Write((ushort)20);
            w.Write((ushort)0);
            w.Write((ushort)0);
            w.Write(0u);
            w.Write(crc);
            w.Write((uint)data.Length);
            w.Write((uint)data.Length);
            w.Write((ushort)nameBytes.Length);
            w.Write((ushort)0);
            w.Write(nameBytes);
            w.Write(data);

            c.Write(0x02014b50u);
            c.Write((ushort)20);
            c.Write((ushort)20);
            c.Write((ushort)0);
            c.Write((ushort)0);
            c.Write(0u);
            c.Write(crc);
            c.Write((uint)data.Length);
            c.Write((uint)data.Length);
            c.Write((ushort)nameBytes.Length);
            c.Write((ushort)0);
            c.Write((ushort)0);
            c.Write((ushort)0);
            c.Write((ushort)0);
            c.Write(0u);
            c.Write(offset);
            c.Write(nameBytes);
        }
        c.Flush();
        var cdOffset = (uint)output.Length;
        w.Write(central.ToArray());
        w.Write(0x06054b50u);
        w.Write((ushort)0);
        w.Write((ushort)0);
        w.Write((ushort)entries.Length);
        w.Write((ushort)entries.Length);
        w.Write((uint)central.Length);
        w.Write(cdOffset);
        w.Write((ushort)0);
        w.Flush();
        return output.ToArray();
    }

    public static byte[] Bytes(string s) => System.Text.Encoding.UTF8.GetBytes(s);
}

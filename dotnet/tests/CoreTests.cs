using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;

namespace Skills.Tests;

public class VersionTests
{
    [Fact]
    public void VersionMatchesProject()
    {
        var csproj = System.Xml.Linq.XDocument.Load(Path.Combine(TestUtil.PortRoot(), "src", "Skills.csproj"));
        Assert.Equal(Program.Version, csproj.Descendants("Version").Single().Value);
    }

    /// When SKILLS_REFERENCE points at a checkout of the TypeScript CLI (as for
    /// the parity harness), the port must claim the same version as that reference.
    [Fact]
    public void VersionMatchesReferenceWhenSet()
    {
        var reference = Environment.GetEnvironmentVariable("SKILLS_REFERENCE");
        if (string.IsNullOrEmpty(reference)) return;
        var pkg = Json.Parse(File.ReadAllText(Path.Combine(reference, "package.json")));
        Assert.Equal(Program.Version, Json.Str(pkg, "version"));
    }

    /// The CLI routes the library core's warnings to stderr and sends the
    /// reference CLI's User-Agent (parity depends on both).
    [Fact]
    public void CliConfiguresTheCore()
    {
        Program.ConfigureCore();
        Assert.Equal($"skills-cli/{Program.Version}", Sys.UserAgent);
        Assert.NotNull(Sys.WarningSink);
    }
}

public class PathTests
{
    [Fact]
    public void PosixSemantics()
    {
        if (OperatingSystem.IsWindows()) return;
        Assert.Equal("/a/c", NodePath.Join("/a/b", "../c"));
        Assert.Equal("a/b/", NodePath.Join("a", "", "b/"));
        Assert.Equal("/a/b", NodePath.Normalize("/a//b/./c/.."));
        Assert.Equal("/a", NodePath.Dirname("/a/b/"));
        Assert.Equal(".", NodePath.Dirname("a"));
        Assert.Equal("/", NodePath.Dirname("/a"));
        Assert.Equal("b", NodePath.Basename("/a/b/"));
        Assert.Equal("../../d", NodePath.Relative("/a/b/c", "/a/d"));
        Assert.Equal("/a/c", NodePath.Resolve("/a", "b", "../c"));
        Assert.Equal(".md", NodePath.Extname("x/SKILL.md"));
        Assert.Equal("", NodePath.Extname(".hidden"));
    }

    [Fact]
    public void Win32Semantics()
    {
        if (!OperatingSystem.IsWindows()) return;
        Assert.Equal(@"C:\a\c", NodePath.Join(@"C:\a\b", "../c"));
        Assert.Equal(@"C:\a\b", NodePath.Normalize("C:/a//b/./c/.."));
        Assert.Equal(@"C:\a", NodePath.Dirname(@"C:\a\b"));
        Assert.Equal(@"C:\", NodePath.Dirname(@"C:\a"));
        Assert.Equal("b", NodePath.Basename(@"C:\a\b\"));
        Assert.Equal(@"..\..\d", NodePath.Relative(@"C:\a\b\c", @"C:\a\d"));
        Assert.Equal(@"D:\b", NodePath.Relative(@"C:\a", @"D:\b"));
        Assert.Equal(@"C:\a\c", NodePath.Resolve(@"C:\a", "b", @"..\c"));
        Assert.True(NodePath.IsAbsolute(@"C:\x"));
        Assert.True(NodePath.IsAbsolute(@"\x"));
        Assert.False(NodePath.IsAbsolute("C:x"));
        Assert.True(NodePath.IsPathSafe(@"C:\a", @"C:\a\b"));
        Assert.False(NodePath.IsPathSafe(@"C:\a", @"C:\a\..\b"));
    }
}

public class SanitizeTests
{
    [Fact]
    public void StripsSequences()
    {
        Assert.Equal("red", Sanitize.StripTerminalEscapes("\x1b[31mred\x1b[0m"));
        Assert.Equal("ab", Sanitize.StripTerminalEscapes("a\x1b]0;title\u0007b"));
        Assert.Equal("alinkb", Sanitize.StripTerminalEscapes("a\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\b"));
        Assert.Equal("xyz", Sanitize.StripTerminalEscapes("x\x1b" + "7y\x1b" + "8z"));
        Assert.Equal("ab", Sanitize.StripTerminalEscapes("a\u009bb"));
        Assert.Equal("a\tb\ncdef", Sanitize.StripTerminalEscapes("a\tb\nc\rd\u0007e\u0008f"));
        Assert.Equal("after", Sanitize.StripTerminalEscapes("\x1bPpayload\x1b\\after"));
    }

    [Fact]
    public void SanitizesMetadata()
    {
        Assert.Equal("multi line name", Sanitize.Metadata("  multi\nline\r\nname  "));
        Assert.Equal("clear", Sanitize.Metadata("\x1b[2Jclear"));
    }
}

public class FrontmatterTests
{
    [Fact]
    public void ParsesFrontmatter()
    {
        var fm = Frontmatter.Parse("---\nname: a\ndescription: b\n---\n# Body\n");
        Assert.Equal("a", Json.Str(fm.Data, "name"));
        Assert.Equal("# Body\n", fm.Content);
        var crlf = Frontmatter.Parse("---\r\nname: a\r\n---\r\nx");
        Assert.Equal("a", Json.Str(crlf.Data, "name"));
        Assert.Equal("x", crlf.Content);
        Assert.Empty(Frontmatter.Parse("no frontmatter").Data);
        Assert.Throws<YamlParseException>(() => Frontmatter.Parse("---\nname: [unclosed\n---\n"));
    }

    [Fact]
    public void AcceptsControlCharactersInScalars()
    {
        var fm = Frontmatter.Parse("---\nname: x\ndescription: Colored \x1b[31mred\x1b[0m text\x07\n---\n");
        Assert.Equal("Colored \x1b[31mred\x1b[0m text\x07", Json.Str(fm.Data, "description"));
        var quoted = Frontmatter.Parse("---\nname: \"a\u0001b\"\ndescription: d\n---\n");
        Assert.Equal("a\u0001b", Json.Str(quoted.Data, "name"));
    }

    [Fact]
    public void StringifiesNestedYaml()
    {
        var v = new JsonObject
        {
            ["name"] = "x",
            ["metadata"] = new JsonObject { ["internal"] = true, ["tags"] = new JsonArray("a", "b") },
            ["version"] = 2,
        };
        Assert.Equal("name: x\nmetadata:\n  internal: true\n  tags:\n    - a\n    - b\nversion: 2\n", Frontmatter.Stringify(v));
        Assert.Equal("description: \"has: colon\"\n", Frontmatter.Stringify(new JsonObject { ["description"] = "has: colon" }));
    }
}

public class UrlTests
{
    [Fact]
    public void EncodeDecode()
    {
        Assert.Equal("a%20b%2Fc%40d", UrlUtil.EncodeUriComponent("a b/c@d"));
        Assert.Equal("%C3%A9", UrlUtil.EncodeUriComponent("é"));
        Assert.Equal("a b", UrlUtil.DecodeUriComponent("a%20b"));
        Assert.Null(UrlUtil.DecodeUriComponent("%E0%A4%A"));
        Assert.Null(UrlUtil.DecodeUriComponent("%zz"));
        Assert.Equal("q=a+b&x=1%262", UrlUtil.SearchParams(("q", "a b"), ("x", "1&2")));
    }

    [Fact]
    public void UrlAccessors()
    {
        var u = WebUrl.Parse("https://Example.com:8443/a/b?x=1")!;
        Assert.Equal("https:", u.Protocol);
        Assert.Equal("example.com", u.Hostname);
        Assert.Equal("example.com:8443", u.Host);
        Assert.Equal("/a/b", u.Pathname);
        Assert.Equal("/", WebUrl.Parse("https://example.com")!.Pathname);
        Assert.Equal("https://h.com/.well-known/agent-skills/s.zip", WebUrl.Parse("s.zip", "https://h.com/.well-known/agent-skills/index.json")!.Href);
    }
}

public class CollateTests
{
    [Fact]
    public void OrdersLikeIcu()
    {
        var v = new[] { "SKILL.md", "references/a.md", "README.md", "_x", "1.txt", "b", "B", "a-b", "a_b", "ab" };
        Assert.Equal(
            ["_x", "1.txt", "a_b", "a-b", "ab", "b", "B", "README.md", "references/a.md", "SKILL.md"],
            Collate.StableSort(v, Collate.LocaleCompare));
    }
}

public class ColorTests
{
    [Fact]
    public void PicocolorsNestedReplacement()
    {
        // bold(dim('x')) must re-open bold after dim's shared close code
        var inner = Pc.Format("x", "\x1b[2m", "\x1b[22m", "\x1b[22m\x1b[2m");
        var outer = Pc.Format(inner, "\x1b[1m", "\x1b[22m", "\x1b[22m\x1b[1m");
        Assert.Equal("\x1b[1m\x1b[2mx\x1b[22m\x1b[1m\x1b[22m", outer);
    }
}

public class JsonTests
{
    [Fact]
    public void IntegerLikeKeysIterateFirst()
    {
        var o = new JsonObject();
        foreach (var k in new[] { "alpha", "10", "007", "9", "beta" }) o[k] = null;
        Assert.Equal(["9", "10", "alpha", "007", "beta"], Json.JsKeyOrder(o).Select(kv => kv.Key));
    }

    [Fact]
    public void StringifyMatchesJsonStringify()
    {
        // JSON.stringify escapes C0 controls but leaves U+2028 as-is.
        var ls = ((char)0x2028).ToString();
        var o = Json.Parse("{\"a\":[1,2.5,\"x\\u2028\\u0001\"],\"b\":{},\"c\":[],\"d\":null,\"e\":1e21}");
        Assert.Equal("{\n  \"a\": [\n    1,\n    2.5,\n    \"x" + ls + "\\u0001\"\n  ],\n  \"b\": {},\n  \"c\": [],\n  \"d\": null,\n  \"e\": 1e+21\n}", Json.Stringify(o));
        Assert.Equal("{\"a\":[1,2.5,\"x" + ls + "\\u0001\"],\"b\":{},\"c\":[],\"d\":null,\"e\":1e+21}", Json.Stringify(o, 0));
    }
}

public class GitTests
{
    [Fact]
    public void ShaAndRefErrors()
    {
        Assert.True(Git.IsCommitSha(new string('a', 40)));
        Assert.False(Git.IsCommitSha("deadbeef"));
        Assert.True(Git.IsMissingRefError("warning: Remote branch abc not found in upstream origin"));
        Assert.True(Git.IsMissingRefError("fatal: couldn't find remote ref abc"));
        Assert.False(Git.IsMissingRefError("fatal: repository not found"));
    }

    [Fact]
    public void GitHubRepoUrls()
    {
        var r = Git.ParseGitHubRepoUrl("https://github.com/o/r.git")!;
        Assert.Equal("o/r", r.Slug);
        Assert.Equal("git@github.com:o/r.git", r.SshUrl);
        Assert.Equal("r", Git.ParseGitHubRepoUrl("git@github.com:o/r.git")!.Repo);
        Assert.Null(Git.ParseGitHubRepoUrl("https://gitlab.com/o/r.git"));
    }

    [Fact]
    public void RejectsExtTransport() => Assert.Throws<GitCloneException>(() => Git.CloneRepo("ext::sh -c touch% /tmp/pwned", null));

    [Fact]
    public void CleanupRefusesOutsideTmp() => Assert.Throws<InvalidOperationException>(() => Git.CleanupTempDir(Sys.HomeDir()));

    [Fact]
    public void ParseInt()
    {
        Assert.Equal(600000, Git.ParseIntPrefix("600000ms"));
        Assert.Null(Git.ParseIntPrefix("abc"));
    }
}

public class LocalLockTests
{
    private static JsonObject Entry(string source, string type, string hash) =>
        new() { ["source"] = source, ["sourceType"] = type, ["computedHash"] = hash };

    [Fact]
    public void RoundtripAndSorting()
    {
        using var t = new TempDir();
        LocalLock.AddSkill("zeta", Entry("owner/repo", "github", "abc"), t.Path);
        LocalLock.AddSkill("alpha", Entry("owner/repo", "github", "abc"), t.Path);
        var text = File.ReadAllText(LocalLock.GetPath(t.Path));
        Assert.True(text.IndexOf("\"alpha\"", StringComparison.Ordinal) < text.IndexOf("\"zeta\"", StringComparison.Ordinal));
        Assert.EndsWith("}\n", text);
        Assert.True(LocalLock.RemoveSkill("alpha", t.Path));
        Assert.Single(LocalLock.Read(t.Path).Skills);
    }

    [Fact]
    public void LocalSourcesArePortable()
    {
        using var t = new TempDir();
        LocalLock.AddSkill("x", Entry(t.Join("skills", "x"), "local", "h"), t.Path);
        Assert.Contains("\"./skills/x\"", File.ReadAllText(LocalLock.GetPath(t.Path)));
        Assert.True(NodePath.IsAbsolute(Json.Str(LocalLock.Read(t.Path).Skills["x"], "source")!));
    }

    [Fact]
    public void FolderHashIsDeterministic()
    {
        using var t = new TempDir();
        t.Write("SKILL.md", "a");
        t.Write("references/x.md", "b");
        // localeCompare order: references/x.md before SKILL.md
        var expected = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes("references/x.mdbSKILL.mda"))).ToLowerInvariant();
        Assert.Equal(expected, LocalLock.ComputeSkillFolderHash(t.Path));
    }
}

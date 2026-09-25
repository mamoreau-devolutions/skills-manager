using System.Formats.Tar;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;

namespace Skills.Tests;

public class SourceParserTests
{
    private static ParsedSource P(string s) => SourceParser.Parse(s);

    [Fact]
    public void GitHubShorthand()
    {
        var s = P("vercel-labs/agent-skills");
        Assert.Equal("github", s.Kind);
        Assert.Equal("https://github.com/vercel-labs/agent-skills.git", s.Url);
        Assert.Null(s.Subpath);
        Assert.Equal("skills/my-skill", P("owner/repo/skills/my-skill").Subpath);
        Assert.Equal("my-skill", P("owner/repo@my-skill").SkillFilter);
        Assert.Equal("https://github.com/owner/repo.git", P("github:owner/repo").Url);
        Assert.Equal("https://github.com/vercel-labs/agent-skills.git", P("vercel-labs/vercel-skills").Url);
    }

    [Fact]
    public void GitHubUrls()
    {
        var s = P("https://github.com/owner/repo/tree/main/skills/x");
        Assert.Equal("github", s.Kind);
        Assert.Equal("main", s.Ref);
        Assert.Equal("skills/x", s.Subpath);
        Assert.Equal("dev", P("https://github.com/owner/repo/tree/dev").Ref);
        var f = P("https://github.com/owner/repo.git#v1.2@skill");
        Assert.Equal("https://github.com/owner/repo.git", f.Url);
        Assert.Equal("v1.2", f.Ref);
    }

    [Fact]
    public void Fragments()
    {
        Assert.Equal("feature/x", P("owner/repo#feature%2Fx").Ref);
        var s = P("owner/repo#main@my-skill");
        Assert.Equal("main", s.Ref);
        Assert.Equal("my-skill", s.SkillFilter);
        // Well-known URLs keep their fragment
        var w = P("https://example.com/docs#section");
        Assert.Equal("well-known", w.Kind);
        Assert.Equal("https://example.com/docs#section", w.Url);
    }

    [Fact]
    public void GitLabAndAzure()
    {
        var s = P("https://gitlab.com/group/sub/repo");
        Assert.Equal("gitlab", s.Kind);
        Assert.Equal("https://gitlab.com/group/sub/repo.git", s.Url);
        var t = P("https://gitlab.example.com/g/r/-/tree/main/skills");
        Assert.Equal("gitlab", t.Kind);
        Assert.Equal("https://gitlab.example.com/g/r.git", t.Url);
        Assert.Equal("skills", t.Subpath);
        Assert.Equal("https://gitlab.com/group/repo.git", P("gitlab:group/repo").Url);
        var a = P("https://dev.azure.com/org/proj/_git/repo?path=/skills&version=GBmain");
        Assert.Equal("git", a.Kind);
        Assert.Equal("https://dev.azure.com/org/proj/_git/repo", a.Url);
        Assert.Equal("main", a.Ref);
        Assert.Equal("skills", a.Subpath);
    }

    [Fact]
    public void DownloadsWellKnownGit()
    {
        Assert.Equal("download", P("https://raw.githubusercontent.com/o/r/main/SKILL.md").Kind);
        Assert.Equal("download", P("https://github.com/o/r/archive/refs/heads/main.zip").Kind);
        Assert.Equal("well-known", P("https://mintlify.com/docs").Kind);
        var s = P("git@github.com:owner/repo.git");
        Assert.Equal("git", s.Kind);
        Assert.Equal("owner/repo", SourceParser.GetOwnerRepo(s));
        Assert.Equal("git", P("https://git.example.com/team/repo.git").Kind);
    }

    [Fact]
    public void LocalPaths()
    {
        var s = P("./skills");
        Assert.Equal("local", s.Kind);
        Assert.True(NodePath.IsAbsolute(s.LocalPath!));
    }

    [Fact]
    public void RejectsTraversal()
    {
        Assert.Throws<SourceParseException>(() => P("owner/repo/../../etc"));
        Assert.Throws<SourceParseException>(() => P("https://github.com/o/r/tree/main/../x"));
    }

    [Fact]
    public void OwnerRepoExtraction()
    {
        Assert.Equal("g/s/r", SourceParser.GetOwnerRepo(P("https://gitlab.com/g/s/r")));
        Assert.Equal("owner/repo", SourceParser.GetOwnerRepo(new ParsedSource("git", "ssh://git@host:7999/owner/repo.git")));
        Assert.Equal(("a", "b"), SourceParser.ParseOwnerRepo("a/b"));
        Assert.Null(SourceParser.ParseOwnerRepo("a/b/c"));
    }
}

public class UpdateSourceTests
{
    [Fact]
    public void GitHubShorthandWithFolderAndRef()
    {
        var e = new UpdateSourceEntry("o/r", "https://github.com/o/r.git", "github", "v1", "skills/x/SKILL.md");
        Assert.Equal("o/r/skills/x#v1", UpdateSource.BuildUpdateInstallSource(e));
        Assert.False(UpdateSource.ShouldUseFullDepthForUpdate(e));
        Assert.Equal("o/r", UpdateSource.BuildUpdateInstallSource(new UpdateSourceEntry("o/r", null, "github", null, "SKILL.md")));
    }

    [Fact]
    public void GenericGitSources()
    {
        var e = new UpdateSourceEntry("team/repo", "https://git.example.com/team/repo.git", "git", null, "skills/x/SKILL.md");
        Assert.Equal("https://git.example.com/team/repo.git", UpdateSource.BuildLocalUpdateSource(e));
        Assert.True(UpdateSource.ShouldUseFullDepthForUpdate(e));
        var legacy = new UpdateSourceEntry("team/repo", null, "git");
        Assert.Null(UpdateSource.BuildLocalUpdateSource(legacy));
        Assert.Null(UpdateSource.BuildLocalCloneSource(legacy));
        Assert.Equal("https://github.com/o/r.git", UpdateSource.BuildLocalCloneSource(new UpdateSourceEntry("o/r", null, "github")));
        var ssh = new UpdateSourceEntry("git@github.com:o/r.git", null, "git", "main", "a/SKILL.md");
        Assert.Equal("git@github.com:o/r.git#main", UpdateSource.BuildLocalUpdateSource(ssh));
    }

    [Fact]
    public void FromJsonTreatsEmptyAsAbsent()
    {
        var e = UpdateSourceEntry.FromJson(Json.Parse("{\"source\":\"o/r\",\"sourceUrl\":\"\",\"ref\":\"\"}"));
        Assert.Null(e.SourceUrl);
        Assert.Null(e.Ref);
    }
}

public class SkillRelocationTests
{
    [Fact]
    public void ResolvesRelocations()
    {
        var lockSkills = (JsonObject)Json.Parse("{\"a\": {\"skillPath\": \"skills/a/SKILL.md\"}, \"b\": {\"skillPath\": \"old/b/SKILL.md\"}, \"c\": {\"skillPath\": \"c/SKILL.md\"}, \"d\": {\"skillPath\": \"d/SKILL.md\"}}")!;
        var discovered = new List<DiscoveredSkillLocation>
        {
            new("a", "skills/a/SKILL.md"),
            new("b", "new/b/SKILL.md"),
            new("d", "x/d/SKILL.md"),
            new("d", "y/d/SKILL.md"),
        };
        var r = SkillRelocation.Resolve(["a", "b", "c", "d"], lockSkills, discovered);
        Assert.Equal("skills/a/SKILL.md", r.ResolvedPaths["a"]);
        Assert.Equal("new/b/SKILL.md", r.ResolvedPaths["b"]);
        Assert.Equal(["c"], r.DeletedSkills);
        Assert.Equal(["d"], r.AmbiguousSkills);
    }
}

public class ArchiveTests
{
    private static readonly ArchiveLimits Limits = new(1 << 20, 100);

    [Fact]
    public void ReadsStoredZip()
    {
        var z = TestUtil.BuildZip(("skill/SKILL.md", TestUtil.Bytes("hello")), ("skill/ref/a.txt", TestUtil.Bytes("x")));
        var files = Archive.ReadZip(z, Limits);
        Assert.Equal(2, files.Count);
        Assert.Equal("skill/SKILL.md", files[0].Path);
        Assert.Equal(TestUtil.Bytes("hello"), files[0].Contents);
    }

    [Fact]
    public void RejectsTraversalAndLimits()
    {
        Assert.Throws<ArchiveValidationException>(() => Archive.ReadZip(TestUtil.BuildZip(("../evil", TestUtil.Bytes("x"))), Limits));
        var two = TestUtil.BuildZip(("a", TestUtil.Bytes("x")), ("b", TestUtil.Bytes("y")));
        Assert.Throws<ArchiveValidationException>(() => Archive.ReadZip(two, new ArchiveLimits(100, 1)));
        Assert.ThrowsAny<Exception>(() => Archive.ReadZip(TestUtil.Bytes("not a zip"), Limits));
    }
}

public class DownloadSourceTests
{
    [Fact]
    public void ArchivePathValidation()
    {
        Assert.Equal("a/b", DownloadSource.ValidateArchivePath("./a/b"));
        Assert.Equal("dir/", DownloadSource.ValidateArchivePath("dir/"));
        Assert.Null(DownloadSource.ValidateArchivePath("/etc/passwd"));
        Assert.Null(DownloadSource.ValidateArchivePath("C:/x"));
        Assert.Null(DownloadSource.ValidateArchivePath("a/../../b"));
    }

    [Fact]
    public void ExtractsZipAndTar()
    {
        var l = DownloadSource.GetLimits(new DownloadOptions());
        using var t = new TempDir();
        var z = TestUtil.BuildZip(("pkg/SKILL.md", TestUtil.Bytes("---\nname: a\ndescription: b\n---\n")));
        Assert.True(DownloadSource.TryExtract(z, t.Path, l));
        Assert.Equal(t.Join("pkg"), DownloadSource.SingleTopLevelDirectory(t.Path));

        using var t2 = new TempDir();
        using var ms = new MemoryStream();
        using (var w = new TarWriter(ms, TarEntryFormat.Gnu, leaveOpen: true))
        {
            var entry = new GnuTarEntry(TarEntryType.RegularFile, "x/SKILL.md") { DataStream = new MemoryStream(TestUtil.Bytes("hello")) };
            w.WriteEntry(entry);
        }
        Assert.True(DownloadSource.TryExtract(ms.ToArray(), t2.Path, l));
        Assert.True(File.Exists(t2.Join("x", "SKILL.md")));
        Assert.False(DownloadSource.TryExtract(TestUtil.Bytes("just some text that is not an archive"), t2.Path, l));
    }
}

public class WellKnownTests
{
    [Fact]
    public void NormalizesV1AndV2()
    {
        var v1 = Json.Parse("{\"skills\": [{\"name\": \"a-b\", \"description\": \"d\", \"files\": [\"SKILL.md\", \"x.txt\"]}]}");
        var e = WellKnown.NormalizeIndex(v1, "https://h.com/docs/.well-known/skills/index.json", ".well-known/skills")!;
        Assert.Equal("https://h.com/docs", Assert.IsType<V1Entry>(e[0]).BaseUrl);
        var badV1 = Json.Parse("{\"skills\": [{\"name\": \"a\", \"description\": \"d\", \"files\": [\"../x\", \"SKILL.md\"]}]}");
        Assert.Null(WellKnown.NormalizeIndex(badV1, "https://h.com/.well-known/skills/index.json", ".well-known/skills"));

        var digest = "sha256:" + new string('a', 64);
        var v2 = Json.Parse($"{{\"$schema\": \"https://schemas.agentskills.io/discovery/0.2.0/schema.json\", \"skills\": [{{\"name\": \"s\", \"type\": \"archive\", \"description\": \"d\", \"url\": \"s.zip\", \"digest\": \"{digest}\"}}, {{\"name\": \"Bad\"}}]}}");
        var e2 = WellKnown.NormalizeIndex(v2, "https://h.com/.well-known/agent-skills/index.json", ".well-known/agent-skills")!;
        Assert.Single(e2);
        Assert.Equal("https://h.com/.well-known/agent-skills/s.zip", Assert.IsType<V2Entry>(e2[0]).ArtifactUrl);
        Assert.Null(WellKnown.NormalizeIndex(Json.Parse("{\"$schema\": \"https://other\", \"skills\": []}"), "https://h.com/i.json", ".well-known/skills"));
    }

    [Fact]
    public void SkillNames()
    {
        Assert.True(WellKnown.IsValidSkillName("my-skill"));
        Assert.False(WellKnown.IsValidSkillName("My-skill"));
        Assert.False(WellKnown.IsValidSkillName("a--b"));
        Assert.False(WellKnown.IsValidSkillName("-a"));
    }

    [Fact]
    public void DigestWithoutIndexDigest()
    {
        var s = new WellKnownSkill
        {
            Name = "n",
            Description = "d",
            Content = "c",
            InstallName = "n",
            SourceUrl = "u",
            Files = [new SnapshotFile("b", TestUtil.Bytes("2")), new SnapshotFile("a", TestUtil.Bytes("1"))],
            IndexEntry = new JsonObject(),
        };
        var expected = "sha256:" + Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes("a\u00001\u0000b\u00002\u0000"))).ToLowerInvariant();
        Assert.Equal(expected, WellKnown.ComputeSkillDigest(s));
        Assert.Equal("example.com", WellKnown.GetSourceIdentifier("https://www.example.com/x"));
    }
}

public class NotionTests
{
    [Fact]
    public void ParsesNotionUrls()
    {
        Assert.Equal("01234567-89ab-cdef-0123-456789abcdef", Notion.ParseSkillUrl("https://www.notion.so/team/My-Skill-0123456789abcdef0123456789abcdef"));
        Assert.Null(Notion.ParseSkillUrl("https://example.com/0123456789abcdef0123456789abcdef"));
        Assert.Null(Notion.ParseSkillUrl("owner/repo"));
        Assert.True(Notion.IsNotionSource("Notion"));
    }

    [Fact]
    public void Selectors() => Assert.Equal("my-pack", Notion.NormalizeSelector("My Pack!"));
}

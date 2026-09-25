using System.Text.Json.Nodes;
using static Skills.Ansi;

namespace Skills.Tests;

// Unit tests for the extensions (preview, validate, pinning, gh skill interop),
// mirroring the Rust port's tests.

public class PreviewTests
{
    private static PreviewFile F(string path, long size) => new(path, size, PreviewCommand.IsScriptPath(path));

    [Fact]
    public void ParsesPreviewOptions()
    {
        var (src, o, errs) = PreviewCommand.ParseOptions(["o/r", "-s", "x", "--file", "a.md", "--json", "--no-pager", "--full-depth"]);
        Assert.Equal(["o/r"], src);
        Assert.Equal("x", o.Skill);
        Assert.Equal("a.md", o.File);
        Assert.True(o.Json && o.NoPager && o.FullDepth);
        Assert.Empty(errs);
        Assert.Equal(["Only one --skill value can be provided"], PreviewCommand.ParseOptions(["o/r", "--skill", "a", "-s", "b"]).Errors);
        Assert.Equal(["Only one --file value can be provided"], PreviewCommand.ParseOptions(["o/r", "--file", "a", "--file", "b"]).Errors);
        Assert.Equal(["-s requires a skill name"], PreviewCommand.ParseOptions(["o/r", "-s"]).Errors);
        Assert.Equal(["--file requires a path"], PreviewCommand.ParseOptions(["o/r", "--file", "--json"]).Errors);
        Assert.Equal(["Unknown option: --bogus"], PreviewCommand.ParseOptions(["--bogus"]).Errors);
    }

    [Fact]
    public void FormatsSizes()
    {
        Assert.Equal("0 B", PreviewCommand.FormatSize(0));
        Assert.Equal("1023 B", PreviewCommand.FormatSize(1023));
        Assert.Equal("1.0 KB", PreviewCommand.FormatSize(1024));
        Assert.Equal("1.5 KB", PreviewCommand.FormatSize(1536));
        Assert.Equal("1.0 KB", PreviewCommand.FormatSize(1075));
        Assert.Equal("1.1 KB", PreviewCommand.FormatSize(1076));
        Assert.Equal("1.0 MB", PreviewCommand.FormatSize(1048576));
        Assert.Equal("1.5 MB", PreviewCommand.FormatSize(1572864));
    }

    [Fact]
    public void DetectsScripts()
    {
        Assert.True(PreviewCommand.IsScriptPath("scripts/run.sh"));
        Assert.True(PreviewCommand.IsScriptPath("Tool.PS1"));
        Assert.False(PreviewCommand.IsScriptPath("notes.md"));
        Assert.False(PreviewCommand.IsScriptPath(".sh"));
        Assert.False(PreviewCommand.IsScriptPath("Makefile"));
    }

    [Fact]
    public void RendersTree()
    {
        var files = PreviewCommand.SortFiles([
            F("scripts/run.sh", 20),
            F("notes.md", 10),
            F("SKILL.md", 1229),
            F("assets/b.png", 2048),
            F("Zeta.md", 1),
            F("scripts/a/deep.txt", 3),
        ]);
        Assert.Equal("SKILL.md", files[0].Path);
        Assert.Equal(
            [
                "  SKILL.md (1.2 KB)",
                "  Zeta.md (1 B)",
                "  notes.md (10 B)",
                "  assets/",
                "    b.png (2.0 KB)",
                "  scripts/",
                "    run.sh (20 B) [script]",
                "    a/",
                "      deep.txt (3 B)",
            ],
            PreviewCommand.RenderTree(files));
    }

    [Fact]
    public void OverviewLayout()
    {
        var text = PreviewCommand.RenderOverview("x", "", [F("SKILL.md", 12), F("run.py", 5)], "---\nname: x\n---\nBody", false);
        var expected = $"{Bold}x{Reset}\n{Dim}{Reset}\n\nFiles:\n  SKILL.md (12 B)\n  run.py (5 B) [script]\n\n{Yellow}⚠ This skill includes 1 script file(s). Review them before installing.{Reset}\n\n{Dim}--- SKILL.md ---{Reset}\n---\nname: x\n---\nBody\n";
        Assert.Equal(expected, text);
    }

    [Fact]
    public void FindsFiles()
    {
        List<PreviewFile> files = [F("SKILL.md", 1), F("Docs/A.md", 1)];
        Assert.Equal("Docs/A.md", PreviewCommand.NormalizeFileArg(".\\Docs\\A.md"));
        Assert.Equal("Docs/A.md", PreviewCommand.FindFile(files, "docs/a.md")!.Path);
        Assert.Null(PreviewCommand.FindFile(files, "nope"));
    }
}

public class ValidateTests
{
    private static List<string> Codes(List<Diagnostic> d) => d.Select(x => x.Code).ToList();

    [Fact]
    public void ParsesValidateOptions()
    {
        var (p, o, e) = ValidateCommand.ParseOptions(["a", "--fix", "--json"]);
        Assert.Equal(["a"], p);
        Assert.True(o.Fix && o.Json && !o.Strict);
        Assert.Empty(e);
        Assert.Equal(["Unknown option: --nope"], ValidateCommand.ParseOptions(["--nope"]).Errors);
    }

    [Fact]
    public void NameRule()
    {
        Assert.True(ValidateCommand.IsValidSkillName("a"));
        Assert.True(ValidateCommand.IsValidSkillName("pdf-tools2"));
        Assert.False(ValidateCommand.IsValidSkillName(""));
        Assert.False(ValidateCommand.IsValidSkillName("-a"));
        Assert.False(ValidateCommand.IsValidSkillName("a-"));
        Assert.False(ValidateCommand.IsValidSkillName("a--b"));
        Assert.False(ValidateCommand.IsValidSkillName("Abc"));
        Assert.False(ValidateCommand.IsValidSkillName("a_b"));
        Assert.True(ValidateCommand.IsValidSkillName(new string('a', 64)));
        Assert.False(ValidateCommand.IsValidSkillName(new string('a', 65)));
    }

    [Fact]
    public void SplitsFrontmatter()
    {
        Assert.Equal(("name: x\n", "body"), ValidateCommand.SplitFrontmatter("---\nname: x\n---\nbody"));
        Assert.Equal(("name: x\r\n", "body\r\n"), ValidateCommand.SplitFrontmatter("---\r\nname: x\r\n---  \r\nbody\r\n"));
        Assert.Equal(("", ""), ValidateCommand.SplitFrontmatter("---\n---\n"));
        Assert.Null(ValidateCommand.SplitFrontmatter("# no fm\n"));
        Assert.Null(ValidateCommand.SplitFrontmatter("---\nname: x\n"));
        Assert.Null(ValidateCommand.SplitFrontmatter("--- \nname: x\n---\n"));
    }

    [Fact]
    public void ChecksFields()
    {
        var (n, d) = ValidateCommand.CheckSkill(
            "---\nname: Bad_Name\ndescription: ''\nallowed-tools: [a]\nextra: 1\nmetadata:\n  github-repo: x\n  n: 1\n---\n", "bad", false, ".");
        Assert.Equal("Bad_Name", n);
        Assert.Equal(
            ["name-format", "name-mismatch", "description-empty", "allowed-tools-type", "metadata-type", "install-metadata", "body-empty", "unknown-field"],
            Codes(d));
        (_, d) = ValidateCommand.CheckSkill("---\nname: x\ndescription: d\n---\nBody\n", "y", true, ".");
        Assert.Equal(["name-mismatch"], Codes(d));
        Assert.Equal(Severity.Warning, d[0].Severity);
        (_, d) = ValidateCommand.CheckSkill("﻿---\n: [\n---\nx", "x", false, ".");
        Assert.Equal(["frontmatter-invalid"], Codes(d));
        (_, d) = ValidateCommand.CheckSkill("no frontmatter", "x", false, ".");
        Assert.Equal(["frontmatter-missing"], Codes(d));
    }

    [Fact]
    public void ExtractsLinks()
    {
        const string body = "See [a](ref/a.md) and ![img](img.png \"t\").\n```\n[x](in-fence.md)\n```\nUse `[y](code.md)` or [a](ref/a.md) [w](https://x.y) [h](#top)\n";
        Assert.Equal(["ref/a.md", "img.png", "https://x.y", "#top"], ValidateCommand.ExtractLinkTargets(body));
        Assert.Null(ValidateCommand.LocalLinkPath("https://x.y"));
        Assert.Null(ValidateCommand.LocalLinkPath("#top"));
        Assert.Null(ValidateCommand.LocalLinkPath("MAILTO:a@b"));
        Assert.Equal("a b.md", ValidateCommand.LocalLinkPath("a%20b.md#s"));
        Assert.Equal("a%zz.md", ValidateCommand.LocalLinkPath("a%zz.md?x"));
        Assert.Null(ValidateCommand.LocalLinkPath("?q"));
    }

    [Fact]
    public void StripsInstallMetadata()
    {
        const string src = "---\nname: x\nmetadata:\n  github-repo: https://github.com/o/r\n  author: me\n  github-path: >-\n    skills/x\n  local-path: /tmp\n---\nbody\n";
        Assert.Equal("---\nname: x\nmetadata:\n  author: me\n---\nbody\n", ValidateCommand.StripInstallMetadata(src));
        const string crlf = "---\r\nname: x\r\nmetadata:\r\n  github-repo: r\r\n  github-ref: refs/heads/main\r\n\r\ndescription: d\r\n---\r\n";
        Assert.Equal("---\r\nname: x\r\ndescription: d\r\n---\r\n", ValidateCommand.StripInstallMetadata(crlf));
        Assert.Null(ValidateCommand.StripInstallMetadata("---\nmetadata: {github-repo: x}\n---\n"));
        Assert.Null(ValidateCommand.StripInstallMetadata("---\nmetadata:\n  author: me\n---\n"));
        const string bom = "﻿---\nmetadata:\n    \"github-sha\": abc\n    k: v\n---\n";
        Assert.Equal("﻿---\nmetadata:\n    k: v\n---\n", ValidateCommand.StripInstallMetadata(bom));
    }

    [Fact]
    public void DuplicateNames()
    {
        List<SkillReport> skills =
        [
            new() { Name = "a", Rel = "x/SKILL.md", Diagnostics = [] },
            new() { Name = "a", Rel = "y/SKILL.md", Diagnostics = [] },
            new() { Name = null, Rel = "z/SKILL.md", Diagnostics = [] },
        ];
        ValidateCommand.AddDuplicateNameDiagnostics(skills);
        Assert.Equal("name \"a\" is also used by y/SKILL.md", skills[0].Diagnostics[0].Message);
        Assert.Single(skills[1].Diagnostics);
        Assert.Empty(skills[2].Diagnostics);
    }
}

public class GhInstalledTests
{
    [Fact]
    public void ParsesRepoUrls()
    {
        Assert.Equal(("github.com", "o", "r"), GhInstalled.ParseRepoUrl("https://github.com/o/r.git/"));
        Assert.Null(GhInstalled.ParseRepoUrl("https://github.com/o"));
        Assert.Null(GhInstalled.ParseRepoUrl("https://github.com/o/r/tree/main"));
        Assert.Null(GhInstalled.ParseRepoUrl("git@github.com:o/r"));
        Assert.Equal("main", GhInstalled.ShortRef("refs/heads/main"));
        Assert.Equal("v1.0", GhInstalled.ShortRef("refs/tags/v1.0"));
        Assert.Equal("abc123", GhInstalled.ShortRef("abc123"));
    }

    [Fact]
    public void ParsesGhMetadata()
    {
        const string md = "---\r\nname: foo\r\ndescription: d\r\nmetadata:\r\n  github-repo: https://github.com/acme/skills\r\n  github-ref: refs/tags/v1.0\r\n  github-tree-sha: abc\r\n  github-path: skills/foo\r\n  github-pinned: v1.0\r\n---\r\nbody\r\n";
        var o = GhInstalled.ParseGhOrigin(md)!;
        Assert.Equal("acme/skills", o.Source());
        Assert.Equal("acme/skills (gh skill, pinned v1.0)", o.ListLabel());
        var g = Assert.IsType<GhGitHubOrigin>(o);
        Assert.Equal("refs/tags/v1.0", g.GitRef);
        Assert.Equal("abc", g.TreeSha);

        const string ghe = "---\nname: x\ndescription: d\nmetadata:\n  github-repo: https://ghe.corp/o/r\n  github-pinned: ''\n---\n";
        Assert.Equal("ghe.corp/o/r (gh skill)", GhInstalled.ParseGhOrigin(ghe)!.ListLabel());

        const string local = "---\nname: x\ndescription: d\nmetadata:\n  local-path: /src/x\n---\n";
        Assert.Equal(new GhLocalOrigin("/src/x"), GhInstalled.ParseGhOrigin(local));
        Assert.Null(GhInstalled.ParseGhOrigin("---\nname: x\nmetadata:\n  author: me\n---\n"));
        Assert.Null(GhInstalled.ParseGhOrigin("no frontmatter"));
        Assert.Null(GhInstalled.ParseGhOrigin("---\nname: [\n---\n"));
    }

    [Fact]
    public void SkillMdPaths()
    {
        Assert.Equal("skills/foo/SKILL.md", GhInstalled.GhSkillMdPath("skills/foo"));
        Assert.Equal("skills/foo/SKILL.md", GhInstalled.GhSkillMdPath("skills/foo/"));
        Assert.Equal("skills/foo/SKILL.md", GhInstalled.GhSkillMdPath("skills/foo/SKILL.md"));
    }

    [Fact]
    public void LockMatching()
    {
        var m = new JsonObject { ["My Skill"] = null };
        Assert.True(GhInstalled.LockHasSkill(m, "My Skill"));
        Assert.True(GhInstalled.LockHasSkill(m, "my-skill"));
        Assert.False(GhInstalled.LockHasSkill(m, "other"));
    }
}

public class GitHubReleaseTests
{
    [Fact]
    public void ReadsReleaseTag()
    {
        Assert.Equal("v1.2.0", GitHubRelease.ReleaseTag(Json.Parse("{\"tag_name\": \"v1.2.0\"}")));
        Assert.Null(GitHubRelease.ReleaseTag(Json.Parse("{\"tag_name\": \"\"}")));
        Assert.Null(GitHubRelease.ReleaseTag(Json.Parse("{}")));
    }
}

public class PinningTests
{
    [Fact]
    public void PinnedEntries()
    {
        var pinned = Json.Parse("{\"source\": \"o/r\", \"ref\": \"v1\", \"pinned\": true, \"x\": 1}");
        Assert.Equal("v1", Pinning.PinnedRef(pinned));
        Assert.False(Pinning.IsPinnedEntry(Json.Parse("{\"ref\": \"v1\"}")));
        Assert.False(Pinning.IsPinnedEntry(Json.Parse("{\"ref\": \"\", \"pinned\": true}")));
        Assert.False(Pinning.IsPinnedEntry(Json.Parse("{\"ref\": \"v1\", \"pinned\": \"true\"}")));
        Assert.Equal("{\"source\":\"o/r\",\"x\":1}", Json.Stringify(Pinning.UnpinnedEntry(pinned), 0));
    }

    [Fact]
    public void PinChecks()
    {
        Assert.Null(Pinning.CheckPin("github", null, "v1"));
        Assert.Null(Pinning.CheckPin("git", "v1", "v1"));
        Assert.Null(Pinning.CheckPin("github", null, "latest"));
        Assert.Equal("--pin is only supported for GitHub, GitLab and Git sources", Pinning.CheckPin("local", null, "v1"));
        Assert.Equal("Conflicting refs: the source selects \"main\" but --pin selects \"v1\". Provide one ref.", Pinning.CheckPin("gitlab", "main", "v1"));
        Assert.Equal("--pin latest is only supported for GitHub sources", Pinning.CheckPin("gitlab", null, "latest"));
    }

    [Fact]
    public void PinActions()
    {
        var pinned = Json.Parse("{\"ref\": \"v1\", \"pinned\": true}");
        var plain = Json.Parse("{\"ref\": \"v1\"}");
        Assert.Equal(new PinAction(PinActionKind.Check), Pinning.GetPinAction(plain, true, true));
        Assert.Equal(new PinAction(PinActionKind.Skip, "v1"), Pinning.GetPinAction(pinned, false, false));
        Assert.Equal(new PinAction(PinActionKind.Force, "v1"), Pinning.GetPinAction(pinned, true, false));
        Assert.Equal(new PinAction(PinActionKind.Unpin), Pinning.GetPinAction(pinned, true, true));
        Assert.Equal(new PinAction(PinActionKind.Unpin), Pinning.GetPinAction(pinned, false, true));
    }

    [Fact]
    public void ParsesAddPinOption()
    {
        var (src, o, errs) = AddCommand.ParseOptions(["x", "--pin", "v1.0", "-y"]);
        Assert.Equal(["x"], src);
        Assert.Equal("v1.0", o.Pin);
        Assert.True(o.Yes);
        Assert.Empty(errs);
        (_, o, errs) = AddCommand.ParseOptions(["x", "--pin", "-y"]);
        Assert.Equal(["--pin requires a ref"], errs);
        Assert.Null(o.Pin);
        Assert.True(o.Yes);
        Assert.Equal(["--pin requires a ref"], AddCommand.ParseOptions(["x", "--pin"]).Errors);
    }
}

namespace Skills.Tests;

public class AgentTests
{
    [Fact]
    public void RegistryShape()
    {
        Assert.Equal(79, Agents.List.Count);
        Assert.Equal("aider-desk", Agents.List[0].Name);
        Assert.Equal("universal", Agents.List[^1].Name);
        Assert.Contains("codex", Agents.GetUniversalAgents());
        Assert.DoesNotContain("replit", Agents.GetUniversalAgents());
        Assert.DoesNotContain("antigravity", Agents.GetVisibleUniversalAgents());
        Assert.Contains("claude-code", Agents.GetNonUniversalAgents());
        Assert.Null(Agents.Get("eve").GlobalSkillsDir);
    }

    [Fact]
    public void OpenClawGlobalDirFallbacks()
    {
        Assert.EndsWith("skills", Agents.GetOpenClawGlobalSkillsDir("/h", _ => false));
        Assert.Contains(".clawdbot", Agents.GetOpenClawGlobalSkillsDir("/h", p => p.EndsWith(".clawdbot")));
    }
}

public class BlobTests
{
    private static RepoTree Tree(params (string Path, string Kind)[] paths) =>
        new("root", "main", paths.Select(p => new TreeEntry(p.Path, p.Kind, $"sha-{p.Path}")).ToList());

    [Fact]
    public void Slugs()
    {
        Assert.Equal("my-cool-skill", Blob.ToSkillSlug("My Cool_Skill!"));
        Assert.Equal("a-b", Blob.ToSkillSlug("--a--b--"));
    }

    [Fact]
    public void FolderHashLookup()
    {
        var t = Tree(("skills/a", "tree"), ("skills/a/SKILL.md", "blob"));
        Assert.Equal("sha-skills/a", Blob.GetSkillFolderHashFromTree(t, "skills/a/SKILL.md"));
        Assert.Equal("root", Blob.GetSkillFolderHashFromTree(t, "SKILL.md"));
        Assert.Null(Blob.GetSkillFolderHashFromTree(t, "missing/SKILL.md"));
    }

    [Fact]
    public void PriorityPaths()
    {
        var t = Tree(("skills/a/SKILL.md", "blob"), ("skills/cat/b/SKILL.md", "blob"), ("skills/a/nested/SKILL.md", "blob"), ("examples/x/SKILL.md", "blob"));
        Assert.Equal(["skills/a/SKILL.md", "skills/cat/b/SKILL.md"], Blob.FindSkillMdPaths(t, null));
        Assert.Equal(["examples/x/SKILL.md"], Blob.FindSkillMdPaths(Tree(("examples/x/SKILL.md", "blob")), null));
        Assert.Equal(["skills/a/SKILL.md", "skills/a/nested/SKILL.md"], Blob.FindSkillMdPaths(t, "skills/a"));
    }
}

public class DiscoveryTests
{
    private static void WriteSkill(TempDir t, string rel, string name) =>
        t.Write(rel.Length == 0 ? "SKILL.md" : $"{rel}/SKILL.md", $"---\nname: {name}\ndescription: d {name}\n---\nbody\n");

    [Fact]
    public void RootSkillReturnsEarly()
    {
        using var t = new TempDir();
        WriteSkill(t, "", "root");
        WriteSkill(t, "skills/child", "child");
        Assert.Single(SkillDiscovery.Discover(t.Path, null, new DiscoverOptions()));
        Assert.Equal(2, SkillDiscovery.Discover(t.Path, null, new DiscoverOptions(FullDepth: true)).Count);
    }

    [Fact]
    public void NestedContainerDepth()
    {
        using var t = new TempDir();
        WriteSkill(t, "skills/cat/sub/deep", "deep");
        WriteSkill(t, "examples/foo", "example");
        // examples/foo sits outside known containers and is only found by the
        // recursive fallback, which does not run once a skill was found.
        Assert.Equal(["deep"], SkillDiscovery.Discover(t.Path, null, new DiscoverOptions()).Select(s => s.Name));
    }

    [Fact]
    public void InternalAndInvalidSkills()
    {
        using var t = new TempDir();
        t.Write("skills/hidden/SKILL.md", "---\nname: hidden\ndescription: x\nmetadata:\n  internal: true\n---\n");
        Assert.Null(SkillDiscovery.ParseSkillMd(t.Join("skills", "hidden", "SKILL.md"), false));
        Assert.NotNull(SkillDiscovery.ParseSkillMd(t.Join("skills", "hidden", "SKILL.md"), true));
        t.Write("bad/SKILL.md", "---\nname: 5\ndescription: x\n---\n");
        Assert.Null(SkillDiscovery.ParseSkillMd(t.Join("bad", "SKILL.md"), false));
    }

    [Fact]
    public void SubpathTraversalRejected()
    {
        using var t = new TempDir();
        Assert.Throws<DiscoverException>(() => SkillDiscovery.Discover(t.Path, "../outside", new DiscoverOptions()));
    }

    [Fact]
    public void FilterMatchesCaseInsensitively()
    {
        var s = new Skill { Name = "My Skill" };
        Assert.Single(SkillDiscovery.Filter([s], ["my skill"]));
        Assert.Empty(SkillDiscovery.Filter([s], ["my"]));
    }

    [Fact]
    public void NormalizeNames() => Assert.Equal("my-skill-name", SkillDiscovery.NormalizeSkillName("My_Skill  Name"));
}

public class InstallerTests
{
    [Fact]
    public void SanitizeNames()
    {
        Assert.Equal("my-skill", Installer.SanitizeName("My Skill"));
        Assert.Equal("etc-passwd", Installer.SanitizeName("../../etc/passwd"));
        Assert.Equal("ce-review", Installer.SanitizeName("ce:review"));
        Assert.Equal("unnamed-skill", Installer.SanitizeName("..."));
        Assert.Equal("hidden", Installer.SanitizeName(".hidden"));
        Assert.Equal("a_b.c", Installer.SanitizeName("a_b.c"));
        Assert.Equal("unnamed-skill", Installer.SanitizeName("日本語"));
        Assert.Equal(255, Installer.SanitizeName(new string('a', 300)).Length);
    }

    [Fact]
    public void EveFrontmatterIsFiltered()
    {
        var raw = "---\nname: x\ndescription: y\nallowed-tools: Bash\nmetadata:\n  internal: true\n---\n\n# Body\n";
        Assert.Equal("---\nname: x\ndescription: y\nmetadata:\n  internal: true\n---\n# Body\n", Installer.StripIgnoredEveFrontmatter(raw));
        Assert.Equal("Body", Installer.StripIgnoredEveFrontmatter("---\nother: 1\n---\nBody"));
    }

    [Fact]
    public void CopyInstallAndSymlinkInstall()
    {
        using var t = new TempDir();
        t.Write("src/my-skill/SKILL.md", "---\nname: my-skill\ndescription: d\n---\n");
        t.Write("src/my-skill/metadata.json", "{}");
        t.Write("src/my-skill/.git/HEAD", "x");
        var project = t.Join("project");
        Directory.CreateDirectory(project);
        var skill = new Skill { Name = "my-skill", Description = "d", Path = t.Join("src", "my-skill") };

        var r = Installer.InstallSkillForAgent(skill, "claude-code", new InstallOptions { Cwd = project, Mode = InstallMode.Symlink });
        Assert.True(r.Success, r.Error);
        var canonical = NodePath.Join(project, ".agents", "skills", "my-skill");
        Assert.Equal(canonical, r.CanonicalPath);
        Assert.True(File.Exists(NodePath.Join(canonical, "SKILL.md")));
        Assert.False(File.Exists(NodePath.Join(canonical, "metadata.json")));
        Assert.False(Directory.Exists(NodePath.Join(canonical, ".git")));
        Assert.True(File.Exists(NodePath.Join(project, ".claude", "skills", "my-skill", "SKILL.md")));

        // Removing the agent link (a junction on Windows) must delete only the
        // link and leave the canonical copy intact.
        var link = NodePath.Join(project, ".claude", "skills", "my-skill");
        Assert.True(Fs.IsSymlink(link));
        Fs.RemoveAll(link);
        Assert.False(Fs.LExists(link));
        Assert.True(File.Exists(NodePath.Join(canonical, "SKILL.md")));

        // Non-universal agent without a project dir is skipped
        var skipped = Installer.InstallSkillForAgent(skill, "windsurf", new InstallOptions { Cwd = project, Mode = InstallMode.Symlink });
        Assert.True(skipped.Skipped);
        Assert.Equal("missing-agent-project-directory", skipped.SkipReason);

        var copied = Installer.InstallSkillForAgent(skill, "windsurf", new InstallOptions { Cwd = project, Mode = InstallMode.Copy });
        Assert.True(copied.Success);
        Assert.True(File.Exists(NodePath.Join(project, ".windsurf", "skills", "my-skill", "SKILL.md")));
    }
}

public class PopulateOnceTests
{
    [Fact]
    public void SharedCanonicalDirIsCopiedOncePerRun()
    {
        using var t = new TempDir();
        t.Write("src/big/SKILL.md", "---\nname: big\ndescription: d\n---\n");
        for (var i = 0; i < 40; i++) t.Write($"src/big/ref/{i}.md", $"file {i}");
        var project = t.Join("project");
        Directory.CreateDirectory(project);
        var skill = new Skill { Name = "big", Description = "d", Path = t.Join("src", "big") };
        var opts = new InstallOptions { Cwd = project, Mode = InstallMode.Symlink };
        var marker = NodePath.Join(project, ".agents", "skills", "big", "ref", "0.md");

        Installer.ResetPopulated();
        Assert.True(Installer.InstallSkillForAgent(skill, "codex", opts).Success);
        Assert.Equal("file 0", File.ReadAllText(marker));
        Assert.Equal(40, Directory.GetFiles(NodePath.Join(project, ".agents", "skills", "big", "ref")).Length);

        // A second universal agent in the same run reuses the populated directory.
        File.WriteAllText(marker, "untouched");
        Assert.True(Installer.InstallSkillForAgent(skill, "cursor", opts).Success);
        Assert.Equal("untouched", File.ReadAllText(marker));

        // A new run starts from scratch and copies again.
        Installer.ResetPopulated();
        Assert.True(Installer.InstallSkillForAgent(skill, "cursor", opts).Success);
        Assert.Equal("file 0", File.ReadAllText(marker));
    }
}

public class UiTests
{
    [Fact]
    public void DetailLinesWrapAndPad()
    {
        Assert.Equal(["one two", "three…"], SearchMultiselect.FormatDetailLines("one two three four five", 9, 2));
        Assert.Equal(["", ""], SearchMultiselect.FormatDetailLines(null, 10, 2));
    }

    [Fact]
    public void VisualRowsCountWrapping()
    {
        Assert.Equal(2, Ui.VisualRowsForLine("abcdef", 3));
        Assert.Equal(1, Ui.VisualRowsForLine("", 3));
        Assert.Equal(1, Ui.VisualRowsForLine("\x1b[31mab\x1b[39m", 2));
    }
}

public class OptionParsingTests
{
    [Fact]
    public void ParsesAddOptions()
    {
        var (src, o, errs) = AddCommand.ParseOptions(["owner/repo", "-a", "claude-code", "cursor", "-s", "x", "-g", "-y", "--copy", "--json", "--metadata", "{\"a\":1}"]);
        Assert.Equal(["owner/repo"], src);
        Assert.Equal(["claude-code", "cursor"], o.Agent);
        Assert.Equal(["x"], o.Skill);
        Assert.True(o.Global);
        Assert.True(o.Yes && o.Copy && o.Json);
        Assert.Equal("{\"a\":1}", o.Metadata);
        Assert.Empty(errs);
        Assert.Equal(["--metadata must be valid JSON"], AddCommand.ParseOptions(["x", "--metadata", "{bad"]).Errors);
        Assert.Equal(["--metadata requires a JSON value"], AddCommand.ParseOptions(["x", "--metadata"]).Errors);
    }

    [Fact]
    public void LockSources()
    {
        Assert.Equal("git@github.com:o/r.git", AddCommand.GetLockSource("git@github.com:o/r.git", "o/r"));
        Assert.Equal("https://gitlab.com/o/r.git", AddCommand.GetLockSource("https://gitlab.com/o/r.git", "o/r"));
        Assert.Equal("o/r", AddCommand.GetLockSource("https://github.com/o/r.git", "o/r"));
        Assert.Equal("u", AddCommand.GetProjectLockSourceUrl("gitlab", "u"));
        Assert.Null(AddCommand.GetProjectLockSourceUrl("github", "u"));
    }

    [Fact]
    public void EvePromptMessage() =>
        Assert.Contains("Install a for your eve agent to use?", Sanitize.StripTerminalEscapes(AddCommand.FormatEveInstallPromptMessage([new Skill { Name = "a" }])));

    [Fact]
    public void ParsesListOptions()
    {
        var o = ListCommand.ParseOptions(["-g", "-a", "cursor", "codex", "--json"]);
        Assert.True(o.Global && o.Json);
        Assert.Equal(["cursor", "codex"], o.Agent);
        Assert.Equal("Document Skills", ListCommand.KebabToTitle("document-skills"));
        Assert.Equal("1, 2, 3, 4, 5 +2 more", ListCommand.FormatList(Enumerable.Range(1, 7).Select(i => i.ToString()).ToList(), 5));
    }

    [Fact]
    public void RemoveResolvesLockKeysFirst()
    {
        Assert.Equal(["ce:review"], RemoveCommand.ResolveSkillsToRemove(["ce:review"], ["ce-review"], ["ce:review"]));
        Assert.Equal(["ce-review"], RemoveCommand.ResolveSkillsToRemove(["CE-Review"], ["ce-review"], []));
        Assert.Empty(RemoveCommand.ResolveSkillsToRemove(["missing"], ["a"], []));
    }

    [Fact]
    public void ParsesRemoveOptions()
    {
        var (skills, o) = RemoveCommand.ParseOptions(["a", "-s", "b", "c", "--agent", "cursor", "-g", "--all", "--bogus"]);
        Assert.Equal(["a", "b", "c"], skills);
        Assert.True(o.Global && o.All && o.Yes);
        Assert.Equal(["cursor"], o.Agent);
    }

    [Fact]
    public void ParsesUseOptions()
    {
        var (src, o, errs) = UseCommand.ParseOptions(["o/r", "--skill", "x", "--agent", "codex"]);
        Assert.Equal(["o/r"], src);
        Assert.Equal("x", o.Skill);
        Assert.Empty(errs);
        Assert.Equal(["skills use --agent does not support '*'; specify exactly one agent."], UseCommand.ParseOptions(["o/r", "--agent", "*"]).Errors);
        Assert.Equal(["--skill requires a skill name"], UseCommand.ParseOptions(["o/r", "--skill"]).Errors);
        Assert.Equal(["Unknown option: --bogus"], UseCommand.ParseOptions(["--bogus"]).Errors);
    }

    [Fact]
    public void BuildsUsePrompt()
    {
        Assert.Equal(
            "You are being given a Skill to execute for the user's next request.\n\nUse the following SKILL.md as your instructions:\n\n<SKILL.md>\nBODY\n</SKILL.md>\n",
            UseCommand.BuildPrompt("BODY", "/tmp/x", false));
        Assert.Contains("Supporting files for this skill were downloaded to:\n/tmp/x", UseCommand.BuildPrompt("BODY", "/tmp/x", true));
    }

    [Fact]
    public void UseSelectorsConflict()
    {
        Assert.Throws<UseFailure>(() => UseCommand.ResolveSelector("a", "b"));
        Assert.Equal("A", UseCommand.ResolveSelector("a", "A"));
    }

    [Fact]
    public void InstallsFormatting()
    {
        Assert.Equal("", FindCommand.FormatInstalls(0));
        Assert.Equal("1 install", FindCommand.FormatInstalls(1));
        Assert.Equal("999 installs", FindCommand.FormatInstalls(999));
        Assert.Equal("1K installs", FindCommand.FormatInstalls(1000));
        Assert.Equal("1.5K installs", FindCommand.FormatInstalls(1540));
        Assert.Equal("2M installs", FindCommand.FormatInstalls(2_000_000));
    }

    [Fact]
    public void FindOptions()
    {
        var (q, o, e) = FindCommand.ParseOptions(["react", "hooks", "--owner", "Vercel"]);
        Assert.Equal("react hooks", q);
        Assert.Equal("vercel", o);
        Assert.Empty(e);
        Assert.Equal(["--owner must be a valid GitHub owner"], FindCommand.ParseOptions(["--owner=bad_owner!"]).Errors);
        Assert.Equal(["--owner requires a GitHub owner"], FindCommand.ParseOptions(["--owner"]).Errors);
    }

    [Fact]
    public void ParsesUpdateOptions()
    {
        var o = UpdateCommand.ParseOptions(["-g", "my-skill", "-y"]);
        Assert.True(o.Global && o.Yes);
        Assert.Equal(["my-skill"], o.Skills);
        Assert.Equal(UpdateScope.Global, UpdateCommand.ResolveScope(o));
        Assert.Equal(UpdateScope.Both, UpdateCommand.ResolveScope(UpdateCommand.ParseOptions(["x"])));
    }

    [Fact]
    public void SkipReasons()
    {
        Assert.Equal("Local path", UpdateCommand.GetSkipReason(Json.Parse("{\"sourceType\": \"local\"}")));
        Assert.Equal("Private or deleted repo", UpdateCommand.GetSkipReason(Json.Parse("{\"sourceType\": \"github\", \"skillFolderHash\": \"\"}")));
        Assert.Equal("No skill path recorded", UpdateCommand.GetSkipReason(Json.Parse("{\"sourceType\": \"github\", \"skillFolderHash\": \"x\"}")));
        Assert.Equal("https://h.com", UpdateCommand.GetInstallSource(new SkippedSkill("n", "r", "https://h.com/.well-known/skills/n/SKILL.md", "well-known", null)));
    }

    [Fact]
    public void ParsesSyncOptions()
    {
        var o = SyncCommand.ParseOptions(["-y", "--force", "-a", "cursor", "codex"]);
        Assert.True(o.Yes && o.Force);
        Assert.Equal(["cursor", "codex"], o.Agent);
    }
}

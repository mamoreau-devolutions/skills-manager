using System.Text.Json.Nodes;
using Devolutions.AgentSkills;

namespace Skills.Tests;

/// Public API tests. Each test gets a sandbox home and project directory, and
/// installs from local sources, so nothing touches the network or the real home.
public sealed class SkillsManagerTests : IDisposable
{
    private readonly TempDir _root = new();
    private readonly string _home;
    private readonly string _project;
    private readonly string _source;

    public SkillsManagerTests()
    {
        _home = _root.Join("home");
        _project = _root.Join("project");
        _source = _root.Join("source");
        Directory.CreateDirectory(_home);
        Directory.CreateDirectory(_project);
        Directory.CreateDirectory(NodePath.Join(_home, ".claude"));
        WriteSkill("skills/alpha", "alpha");
        WriteSkill("skills/beta", "beta");
        File.WriteAllText(NodePath.Join(_source, "skills/alpha/notes.txt"), "extra file");
    }

    public void Dispose() => _root.Dispose();

    private void WriteSkill(string rel, string name, string? description = null)
    {
        var dir = NodePath.Join(_source, rel);
        Directory.CreateDirectory(dir);
        var desc = description == null ? "" : $"description: {description}\n";
        if (description == null) desc = $"description: The {name} skill\n";
        File.WriteAllText(NodePath.Join(dir, "SKILL.md"), $"---\nname: {name}\n{desc}---\n# {name}\n");
    }

    private SkillsManager Manager() => new(new SkillsManagerOptions { HomeDirectory = _home, ProjectDirectory = _project });

    private sealed class Lines : IProgress<string>
    {
        public readonly List<string> Items = [];

        public void Report(string value)
        {
            lock (Items) Items.Add(value);
        }
    }

    [Fact]
    public void AgentsReflectTheSandboxHome()
    {
        var agents = Manager().GetAgents();
        var claude = agents.Single(a => a.Id == "claude-code");
        Assert.Equal("Claude Code", claude.DisplayName);
        Assert.Equal(".claude/skills", claude.ProjectSkillsDirectory);
        Assert.Equal(NodePath.Join(_home, ".claude", "skills"), claude.GlobalSkillsDirectory);
        Assert.True(claude.IsDetected);
        Assert.False(claude.IsUniversal);
        Assert.False(agents.Single(a => a.Id == "cursor").IsDetected);
        Assert.True(agents.Single(a => a.Id == "codex").IsUniversal);
        // The sandbox never leaks into process-wide state.
        Assert.Null(Sys.Context);
        Assert.NotEqual(_home, Agents.Home);
    }

    [Fact]
    public void ListsAvailableSkillsWithoutInstalling()
    {
        var available = Manager().GetAvailableSkills(_source);
        Assert.Equal(["alpha", "beta"], available.Select(s => s.Name).Order());
        Assert.Equal("The alpha skill", available.Single(s => s.Name == "alpha").Description);
        Assert.False(Directory.Exists(NodePath.Join(_home, ".agents")));
    }

    [Fact]
    public void GlobalInstallLinksAgentsAndListsTheSkill()
    {
        var m = Manager();
        var result = m.Install(new SkillInstallRequest { Source = _source, Skills = ["alpha"], Agents = ["claude-code", "cursor"] });

        var outcome = Assert.Single(result.Skills);
        Assert.True(result.Succeeded);
        Assert.Equal("alpha", outcome.Name);
        Assert.Equal(SkillInstallMode.Symlink, outcome.Mode);
        Assert.Equal(NodePath.Join(_home, ".agents", "skills", "alpha"), outcome.Path);
        Assert.Equal(["claude-code", "cursor"], outcome.Agents.Order());
        Assert.True(File.Exists(NodePath.Join(_home, ".agents", "skills", "alpha", "notes.txt")));
        Assert.True(File.Exists(NodePath.Join(_home, ".claude", "skills", "alpha", "SKILL.md")));
        // Universal agents (Cursor) read ~/.agents/skills directly: no link of their own.
        Assert.False(Directory.Exists(NodePath.Join(_home, ".cursor", "skills", "alpha")));

        var installed = m.GetInstalledSkills(SkillScope.Global);
        var alpha = Assert.Single(installed, s => s.Name == "alpha");
        Assert.Equal(SkillScope.Global, alpha.Scope);
        Assert.Equal("The alpha skill", alpha.Description);
        Assert.Contains("claude-code", alpha.Agents);
        // Local sources are not tracked in the global lock (as with `skills add -g`).
        Assert.False(alpha.IsTracked);
        Assert.Empty(m.GetInstalledSkills(SkillScope.Project));
    }

    [Fact]
    public void ProjectInstallWritesSkillsLock()
    {
        var m = Manager();
        var result = m.Install(new SkillInstallRequest { Source = _source, Scope = SkillScope.Project, Agents = ["claude-code"] });
        Assert.True(result.Succeeded);
        Assert.Equal(["alpha", "beta"], result.Skills.Select(s => s.Name).Order());
        Assert.All(result.Skills, s => Assert.NotNull(s.Hash));

        var lockFile = JsonNode.Parse(File.ReadAllText(NodePath.Join(_project, "skills-lock.json")))!;
        var entry = lockFile["skills"]!["alpha"]!;
        Assert.Equal("local", (string?)entry["sourceType"]);
        Assert.Equal(result.Skills.Single(s => s.Name == "alpha").Hash, (string?)entry["computedHash"]);
        Assert.True(File.Exists(NodePath.Join(_project, ".claude", "skills", "beta", "SKILL.md")));

        var alpha = Assert.Single(m.GetInstalledSkills(SkillScope.Project), s => s.Name == "alpha");
        Assert.Equal("local", alpha.SourceType);
        Assert.Equal((string?)entry["computedHash"], alpha.Hash);
    }

    [Fact]
    public void MissingSkillsAreReportedOrRejected()
    {
        var m = Manager();
        var partial = m.Install(new SkillInstallRequest { Source = _source, Skills = ["alpha", "gamma"], Agents = ["claude-code"] });
        Assert.False(partial.Succeeded);
        Assert.Equal(SkillOperationStatus.NotFound, partial.Skills.Single(s => s.Name == "gamma").Status);
        Assert.Equal(SkillOperationStatus.Succeeded, partial.Skills.Single(s => s.Name == "alpha").Status);

        var e = Assert.Throws<SkillsException>(() => m.Install(new SkillInstallRequest { Source = _source, Skills = ["gamma"] }));
        Assert.Contains("gamma", e.Message);
        Assert.Throws<SkillsException>(() => m.Install(new SkillInstallRequest { Source = _root.Join("nowhere") }));
        Assert.Throws<SkillsException>(() => m.Install(new SkillInstallRequest { Source = "notion" }));
    }

    [Fact]
    public void UnknownAgentsAreRejectedBeforeFetching()
    {
        var e = Assert.Throws<ArgumentException>(() =>
            Manager().Install(new SkillInstallRequest { Source = _root.Join("nowhere"), Agents = ["not-an-agent"] }));
        Assert.Contains("not-an-agent", e.Message);
    }

    [Fact]
    public void DefaultAgentsAreDetectedPlusUniversal()
    {
        var result = Manager().Install(new SkillInstallRequest { Source = _source, Skills = ["beta"] });
        var agents = Assert.Single(result.Skills).Agents;
        Assert.Contains("claude-code", agents);
        Assert.Contains("codex", agents);
        Assert.Contains("cursor", agents);
        Assert.DoesNotContain("windsurf", agents);
        Assert.DoesNotContain("eve", agents);
    }

    [Fact]
    public void InvalidSkillFilesAreReportedAsProgress()
    {
        var dir = NodePath.Join(_source, "skills", "broken");
        Directory.CreateDirectory(dir);
        File.WriteAllText(NodePath.Join(dir, "SKILL.md"), "---\nname: broken\n---\nno description\n");
        var lines = new Lines();
        Manager().GetAvailableSkills(_source, progress: lines);
        Assert.Contains(lines.Items, l => l.Contains("Skipped") && l.Contains("description"));
    }

    [Fact]
    public void RemoveDeletesLinksCanonicalCopyAndLockEntry()
    {
        var m = Manager();
        m.Install(new SkillInstallRequest { Source = _source, Scope = SkillScope.Project, Agents = ["claude-code"] });

        var result = m.Remove(["alpha", "missing"], SkillScope.Project);
        Assert.Equal(SkillOperationStatus.Succeeded, result.Skills.Single(s => s.Name == "alpha").Status);
        Assert.Equal(SkillOperationStatus.NotFound, result.Skills.Single(s => s.Name == "missing").Status);
        // One target directory: skills were copied straight into .claude/skills.
        Assert.False(Directory.Exists(NodePath.Join(_project, ".claude", "skills", "alpha")));
        Assert.True(Directory.Exists(NodePath.Join(_project, ".claude", "skills", "beta")));
        var lockFile = JsonNode.Parse(File.ReadAllText(NodePath.Join(_project, "skills-lock.json")))!;
        Assert.Null(lockFile["skills"]!["alpha"]);
        Assert.NotNull(lockFile["skills"]!["beta"]);
        Assert.Equal(["beta"], m.GetInstalledSkills(SkillScope.Project).Select(s => s.Name));
    }

    [Fact]
    public void UpdateCheckReportsUncheckableSkills()
    {
        var m = Manager();
        Directory.CreateDirectory(NodePath.Join(_home, ".agents"));
        File.WriteAllText(NodePath.Join(_home, ".agents", ".skill-lock.json"), """
            {"version":3,"skills":{"from-disk":{"source":"/some/dir","sourceType":"local","sourceUrl":"/some/dir","skillFolderHash":""},"held":{"source":"owner/repo","sourceType":"github","sourceUrl":"https://github.com/owner/repo.git","ref":"v1.2.0","pinned":true,"skillPath":"skills/held/SKILL.md","skillFolderHash":"abc"}},"dismissed":{}}
            """);
        File.WriteAllText(NodePath.Join(_project, "skills-lock.json"), """
            {"version":1,"skills":{"legacy":{"source":"owner/repo","sourceType":"github","computedHash":"abc"}}}
            """);

        var check = m.CheckForUpdates();
        Assert.Empty(check.Updates);
        var local = Assert.Single(check.Unchecked, u => u.Name == "from-disk");
        Assert.Equal(SkillScope.Global, local.Scope);
        Assert.Equal("Local path", local.Reason);
        var held = Assert.Single(check.Unchecked, u => u.Name == "held");
        Assert.Equal("Pinned to v1.2.0", held.Reason);
        var legacy = Assert.Single(check.Unchecked, u => u.Name == "legacy");
        Assert.Equal(SkillScope.Project, legacy.Scope);
        Assert.Equal("owner/repo", legacy.Source);

        Assert.Equal(2, m.CheckForUpdates(SkillScope.Global).Unchecked.Count);
        Assert.Empty(m.CheckForUpdates(SkillScope.Global, ["other"]).Unchecked);
        Assert.Empty(m.Update(SkillScope.Global).Skills);
    }

    [Fact]
    public async Task CancellationIsHonored()
    {
        using var cts = new CancellationTokenSource();
        cts.Cancel();
        var m = Manager();
        Assert.Throws<OperationCanceledException>(() => m.GetInstalledSkills(cancellationToken: cts.Token));
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => m.InstallAsync(new SkillInstallRequest { Source = _source }, cancellationToken: cts.Token));
        Assert.False(Directory.Exists(NodePath.Join(_home, ".agents", "skills")));
    }

    [Fact]
    public async Task ConcurrentCallsKeepTheirOwnSandbox()
    {
        using var other = new TempDir();
        Directory.CreateDirectory(other.Join("home"));
        var m2 = new SkillsManager(new SkillsManagerOptions { HomeDirectory = other.Join("home"), ProjectDirectory = other.Path });
        var a = Manager().InstallAsync(new SkillInstallRequest { Source = _source, Skills = ["alpha"], Agents = ["codex"] });
        var b = m2.InstallAsync(new SkillInstallRequest { Source = _source, Skills = ["beta"], Agents = ["codex"] });
        await Task.WhenAll(a, b);
        Assert.True(Directory.Exists(NodePath.Join(_home, ".agents", "skills", "alpha")));
        Assert.False(Directory.Exists(NodePath.Join(_home, ".agents", "skills", "beta")));
        Assert.True(Directory.Exists(other.Join("home", ".agents", "skills", "beta")));
    }

    /// Directories keep the form the caller gave (path.resolve semantics, as in
    /// the CLI): an 8.3 short name such as C:\Users\RUNNER~1 is not expanded.
    [Fact]
    public void ShortNamePathsAreKeptAsGiven()
    {
        if (!OperatingSystem.IsWindows()) return;
        var longDir = NodePath.Join(_root.Path, "a-long-directory-name");
        Directory.CreateDirectory(longDir);
        var psi = new System.Diagnostics.ProcessStartInfo("cmd.exe", $"/c for %I in (\"{longDir}\") do @echo %~sI")
        {
            RedirectStandardOutput = true,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        using var p = System.Diagnostics.Process.Start(psi)!;
        var shortDir = p.StandardOutput.ReadToEnd().Trim();
        p.WaitForExit();
        if (shortDir.Length == 0 || shortDir.Equals(longDir, StringComparison.OrdinalIgnoreCase)) return; // 8.3 names disabled

        var m = new SkillsManager(new SkillsManagerOptions { HomeDirectory = shortDir, ProjectDirectory = shortDir });
        Assert.Equal(shortDir, m.ProjectDirectory);
        Assert.Equal(NodePath.Join(shortDir, ".claude", "skills"), m.GetAgents().Single(a => a.Id == "claude-code").GlobalSkillsDirectory);
    }

    [Fact]
    public void VersionMatchesThePackage()
    {
        var csproj = File.ReadAllText(NodePath.Join(TestUtil.PortRoot(), "lib", "Devolutions.AgentSkills", "Devolutions.AgentSkills.csproj"));
        Assert.Contains($"<Version>{SkillsManager.Version}</Version>", csproj);
    }
}

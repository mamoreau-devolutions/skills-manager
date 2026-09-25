// `skills remove` (port of remove.ts).

namespace Skills;

internal sealed class RemoveOptions
{
    public bool Global { get; set; }
    public List<string>? Agent { get; set; }
    public bool Yes { get; set; }
    public bool All { get; set; }
}

internal static class RemoveCommand
{
    /// Resolve requested names to canonical removal targets, preferring lock keys.
    public static List<string> ResolveSkillsToRemove(IEnumerable<string> requested, IEnumerable<string> folderNames, IEnumerable<string> lockKeys)
    {
        var identity = new Dictionary<string, string>();
        foreach (var f in folderNames) identity[Installer.SanitizeName(f)] = f;
        foreach (var k in lockKeys) identity[Installer.SanitizeName(k)] = k;
        var matched = new List<string>();
        foreach (var name in requested)
            if (identity.TryGetValue(Installer.SanitizeName(name), out var hit) && !matched.Contains(hit))
                matched.Add(hit);
        return matched;
    }

    private sealed record Result(string Skill, bool Success, string Source, string SourceType, string? Error);

    public static void Run(List<string> skillNames, RemoveOptions options)
    {
        Installer.ResetPopulated();
        var agentResult = DetectAgent.Detect();
        if (agentResult.IsAgent)
        {
            options.Yes = true;
            Ui.Log.Info($"{Pc.BgCyan(Pc.Black(Pc.Bold($" {agentResult.Name} ")))} Agent detected — removing non-interactively");
        }

        if (skillNames.Contains("*"))
        {
            options.All = true;
            skillNames.RemoveAll(n => n == "*");
        }
        if (options.All && skillNames.Count > 0)
        {
            Ui.Log.Error("Cannot combine --all with specific skill names.");
            Ui.Log.Info("Use `skills remove --all` to remove every skill, or omit --all to remove only the named skills.");
            Ui.Log.Info($"Example: skills remove {skillNames[0]} -y");
            Sys.Exit(1);
        }

        var isGlobal = options.Global;
        var cwd = Sys.Cwd();
        var spinner = new Ui.Spinner();
        spinner.Start("Scanning for installed skills…");

        var found = new List<string>();
        void ScanDir(string dir)
        {
            List<DirEntry> entries;
            try
            {
                entries = Fs.ReadDir(dir);
            }
            catch (Exception e) when (e is DirectoryNotFoundException or FileNotFoundException)
            {
                return;
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                Ui.Log.Warn($"Could not scan directory {dir}: {e.Message}");
                return;
            }
            foreach (var e in entries)
            {
                if (!e.IsDirectory || e.Name.StartsWith('.')) continue;
                if (!SkillDiscovery.HasSkillMd(e.FullPath)) continue;
                if (!found.Contains(e.Name)) found.Add(e.Name);
            }
        }

        if (isGlobal)
        {
            ScanDir(Installer.GetCanonicalSkillsDir(true, cwd));
            foreach (var a in Agents.List)
                if (a.GlobalSkillsDir != null) ScanDir(a.GlobalSkillsDir);
        }
        else
        {
            ScanDir(Installer.GetCanonicalSkillsDir(false, cwd));
            foreach (var a in Agents.List) ScanDir(NodePath.Join(cwd, a.SkillsDir));
            foreach (var sub in Agents.GetEveSubagents(cwd)) ScanDir(Installer.GetEveSubagentSkillsDir(sub, cwd));
        }

        var installed = Collate.StableSort(found, string.CompareOrdinal);
        spinner.Stop($"Found {installed.Count} unique installed skill(s)");

        var lockKeys = (isGlobal ? SkillLock.Read().Skills : LocalLock.Read(cwd).Skills).Select(kv => kv.Key).ToList();

        var requested = options.All ? installed.Concat(lockKeys).ToList() : skillNames;
        var resolved = options.All || skillNames.Count > 0 ? ResolveSkillsToRemove(requested, installed, lockKeys) : [];

        if (installed.Count == 0 && resolved.Count == 0)
        {
            Ui.Outro(Pc.Yellow("No skills found to remove."));
            return;
        }

        if (options.Agent is { Count: > 0 } agentList)
        {
            var invalid = agentList.Where(a => Agents.Find(a) == null).ToList();
            if (invalid.Count > 0)
            {
                Ui.Log.Error($"Invalid agents: {string.Join(", ", invalid)}");
                Ui.Log.Info($"Valid agents: {string.Join(", ", Agents.AllNames())}");
                Sys.Exit(1);
            }
        }

        List<string> selected;
        if (options.All)
        {
            selected = resolved;
        }
        else if (skillNames.Count > 0)
        {
            if (resolved.Count == 0)
            {
                Ui.Log.Error($"No matching skills found for: {string.Join(", ", skillNames)}");
                return;
            }
            selected = resolved;
        }
        else
        {
            var choices = installed.Select(s => new SelectOption<string>(s, s)).ToList();
            var chosen = Ui.Multiselect($"Select skills to remove {Pc.Dim("(space to toggle)")}", choices, [], true);
            if (chosen == null)
            {
                Ui.Cancel("Removal cancelled");
                Sys.Exit(0);
            }
            selected = ResolveSkillsToRemove(chosen, installed, lockKeys);
        }

        List<string> targetAgents;
        if (options.Agent is { Count: > 0 } al)
        {
            targetAgents = al;
        }
        else
        {
            targetAgents = Agents.AllNames();
            spinner.Stop($"Targeting {targetAgents.Count} potential agent(s)");
        }

        if (!options.Yes)
        {
            Sys.OutLine();
            Ui.Log.Info("Skills to remove:");
            foreach (var s in selected) Ui.Log.Message($"  {Pc.Red("•")} {s}");
            Sys.OutLine();
            if (Ui.Confirm($"Are you sure you want to uninstall {selected.Count} skill(s)?") != true)
            {
                Ui.Cancel("Removal cancelled");
                Sys.Exit(0);
            }
        }

        spinner.Start("Removing skills…");

        var results = new List<Result>();
        foreach (var skillName in selected)
        {
            try
            {
                var canonical = Installer.GetCanonicalPath(skillName, isGlobal, cwd);
                foreach (var at in targetAgents)
                {
                    var a = Agents.Get(at);
                    var skillPath = Installer.GetInstallPath(skillName, at, isGlobal, cwd);
                    var sanitized = Installer.SanitizeName(skillName);
                    var cleanup = new List<string> { skillPath };
                    void Add(string p)
                    {
                        if (!cleanup.Contains(p)) cleanup.Add(p);
                    }
                    if (isGlobal && a.GlobalSkillsDir != null)
                    {
                        Add(NodePath.Join(a.GlobalSkillsDir, sanitized));
                    }
                    else
                    {
                        Add(NodePath.Join(cwd, a.SkillsDir, sanitized));
                        if (at == "eve")
                            foreach (var sub in Agents.GetEveSubagents(cwd)) Add(NodePath.Join(Installer.GetEveSubagentSkillsDir(sub, cwd), sanitized));
                    }
                    foreach (var p in cleanup)
                    {
                        if (p == canonical || !Fs.LExists(p)) continue;
                        try
                        {
                            Fs.RemoveAll(p);
                        }
                        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
                        {
                            Ui.Log.Warn($"Could not remove skill from {a.DisplayName}: {e.Message}");
                        }
                    }
                }

                var remaining = Agents.DetectInstalledAgents().Where(a => !targetAgents.Contains(a));
                var stillUsed = remaining.Any(at => Fs.LExists(Installer.GetInstallPath(skillName, at, isGlobal, cwd)));
                if (!stillUsed) Fs.RemoveAll(canonical);

                var entry = isGlobal ? SkillLock.GetSkill(skillName) : LocalLock.Read(cwd).Skills[skillName];
                var source = Json.NonEmpty(entry, "source") ?? "local";
                var sourceType = Json.NonEmpty(entry, "sourceType") ?? "local";
                if (!stillUsed)
                {
                    if (isGlobal) SkillLock.RemoveSkill(skillName);
                    else LocalLock.RemoveSkill(skillName, cwd);
                }
                results.Add(new Result(skillName, true, source, sourceType, null));
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException or InvalidOperationException)
            {
                results.Add(new Result(skillName, false, "", "", e.Message));
            }
        }

        spinner.Stop("Removal process complete");

        var successful = results.Where(r => r.Success).ToList();
        var failed = results.Where(r => !r.Success).ToList();

        if (successful.Count > 0)
        {
            var bySource = new List<(string Source, List<string> Skills, string SourceType)>();
            foreach (var r in successful)
            {
                var src = r.Source.Length == 0 ? "local" : r.Source;
                var i = bySource.FindIndex(x => x.Source == src);
                if (i >= 0)
                {
                    bySource[i].Skills.Add(r.Skill);
                    bySource[i] = bySource[i] with { SourceType = r.SourceType };
                }
                else
                {
                    bySource.Add((src, [r.Skill], r.SourceType));
                }
            }
            foreach (var (source, skills, sourceType) in bySource)
            {
                Telemetry.Track(
                    ("event", "remove"),
                    ("source", source),
                    ("skills", string.Join(",", skills)),
                    ("agents", string.Join(",", targetAgents)),
                    ("global", isGlobal ? "1" : null),
                    ("sourceType", sourceType));
            }
            Ui.Log.Success(Pc.Green($"Successfully removed {successful.Count} skill(s)"));
        }
        if (failed.Count > 0)
        {
            Ui.Log.Error(Pc.Red($"Failed to remove {failed.Count} skill(s)"));
            foreach (var r in failed) Ui.Log.Message($"  {Pc.Red("✗")} {r.Skill}: {r.Error}");
        }
        Sys.OutLine();
        Ui.Outro(Pc.Green("Done!"));
    }

    /// Separate skill names from option flags.
    public static (List<string> Skills, RemoveOptions Options) ParseOptions(IReadOnlyList<string> args)
    {
        var o = new RemoveOptions();
        var skills = new List<string>();
        for (var i = 0; i < args.Count; i++)
        {
            var a = args[i];
            switch (a)
            {
                case "-g" or "--global":
                    o.Global = true;
                    break;
                case "-y" or "--yes":
                    o.Yes = true;
                    break;
                case "--all":
                    o.All = true;
                    o.Yes = true;
                    break;
                case "-s" or "--skill":
                    while (i + 1 < args.Count && args[i + 1].Length > 0 && !args[i + 1].StartsWith('-')) skills.Add(args[++i]);
                    break;
                case "-a" or "--agent":
                    o.Agent ??= [];
                    while (i + 1 < args.Count && args[i + 1].Length > 0 && !args[i + 1].StartsWith('-')) o.Agent.Add(args[++i]);
                    break;
                default:
                    if (a.Length > 0 && !a.StartsWith('-')) skills.Add(a);
                    break;
            }
        }
        return (skills, o);
    }
}

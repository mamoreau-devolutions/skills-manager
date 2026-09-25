// `skills update` / `check` / `upgrade` (port of update.ts).
//
// Changed skills are reinstalled by invoking this executable's own `add`
// command (the TS CLI runs `node <repo>/bin/cli.mjs add ...`), never through
// a shell.

using System.Text.Json.Nodes;
using static Skills.Ansi;

namespace Skills;

internal enum UpdateScope
{
    Project,
    Global,
    Both,
}

internal sealed class UpdateOptions
{
    public bool Global { get; set; }
    public bool Project { get; set; }
    public bool Yes { get; set; }
    public List<string>? Skills { get; set; }

    // Extensions: report without changing anything / reinstall even when
    // current / include pinned skills and clear their pin.
    public bool DryRun { get; set; }
    public bool Force { get; set; }
    public bool Unpin { get; set; }
}

internal sealed record SkippedSkill(string Name, string Reason, string SourceUrl, string SourceType, string? Ref);

internal static class UpdateCommand
{
    private sealed record ProjectSkill(string Name, JsonNode Entry);

    /// `WellKnownOutcome.Pending`: changed skills as (name, base URL) under `--dry-run`.
    private sealed record WellKnownOutcome(int Ok, int Fail, bool ChangedAny, List<(string Name, string Source)> Pending);

    private sealed record WellKnownItem(string Name, string Digest, List<string>? Subagents);

    private sealed record Resolution(List<string> Deleted, Dictionary<string, string> Resolved);

    private sealed record WellKnownCheck(bool Changed, List<string> ChangedSkills, List<string> Removed, List<string> NewSkills);

    private static string ScopeName(UpdateScope s) => s switch
    {
        UpdateScope.Project => "project",
        UpdateScope.Global => "global",
        _ => "both",
    };

    public static UpdateOptions ParseOptions(IReadOnlyList<string> args)
    {
        var o = new UpdateOptions();
        var positional = new List<string>();
        foreach (var a in args)
        {
            switch (a)
            {
                case "-g" or "--global":
                    o.Global = true;
                    break;
                case "-p" or "--project":
                    o.Project = true;
                    break;
                case "-y" or "--yes":
                    o.Yes = true;
                    break;
                case "--dry-run":
                    o.DryRun = true;
                    break;
                case "--force":
                    o.Force = true;
                    break;
                case "--unpin":
                    o.Unpin = true;
                    break;
                default:
                    if (!a.StartsWith('-')) positional.Add(a);
                    break;
            }
        }
        if (positional.Count > 0) o.Skills = positional;
        return o;
    }

    /// Whether cwd has project skills (a lock file or a skill in .agents/skills).
    public static bool HasProjectSkills(string? cwd = null)
    {
        var dir = cwd ?? Sys.Cwd();
        if (Fs.Exists(NodePath.Join(dir, "skills-lock.json"))) return true;
        var skillsDir = NodePath.Join(dir, ".agents", "skills");
        return Fs.TryReadDir(skillsDir).Any(e => e.IsDirectory && Fs.Exists(NodePath.Join(skillsDir, e.Name, "SKILL.md")));
    }

    public static UpdateScope ResolveScope(UpdateOptions o)
    {
        if (o.Skills is { Count: > 0 }) return o.Global ? UpdateScope.Global : o.Project ? UpdateScope.Project : UpdateScope.Both;
        if (o.Global && o.Project) return UpdateScope.Both;
        if (o.Global) return UpdateScope.Global;
        if (o.Project) return UpdateScope.Project;
        if (o.Yes || !Sys.StdinIsTty()) return HasProjectSkills() ? UpdateScope.Project : UpdateScope.Global;
        var options = new List<SelectOption<UpdateScope>>
        {
            new(UpdateScope.Project, "Project", "Update skills in current directory"),
            new(UpdateScope.Global, "Global", "Update skills in home directory"),
            new(UpdateScope.Both, "Both", "Update all skills"),
        };
        if (Ui.Select("Update scope", options, 0, out var scope)) return scope;
        Ui.Cancel("Cancelled");
        Sys.Exit(0);
        return default;
    }

    public static bool MatchesSkillFilter(string name, List<string>? filter) =>
        filter is not { Count: > 0 } || filter.Any(x => x.ToLowerInvariant() == name.ToLowerInvariant());

    private static string? S(JsonNode? e, string k) => Json.Str(e, k);

    private static string? NonEmpty(JsonNode? e, string k) => Json.NonEmpty(e, k);

    public static string GetSkipReason(JsonNode? e) => S(e, "sourceType") switch
    {
        "local" => "Local path",
        "git" => "Git URL",
        "well-known" => "Well-known skill",
        _ => NonEmpty(e, "skillFolderHash") == null ? "Private or deleted repo"
            : NonEmpty(e, "skillPath") == null ? "No skill path recorded"
            : "No version tracking",
    };

    public static string GetInstallSource(SkippedSkill skill)
    {
        var url = skill.SourceUrl;
        if (skill.SourceType == "well-known" && url.IndexOf("/.well-known/", StringComparison.Ordinal) is var i and >= 0) url = url[..i];
        return UpdateSource.FormatSourceInput(url, skill.Ref);
    }

    public static void PrintSkippedSkills(List<SkippedSkill> skipped)
    {
        if (skipped.Count == 0) return;
        Sys.OutLine();
        Sys.OutLine($"{Dim}{skipped.Count} skill(s) cannot be checked automatically:{Reset}");
        var grouped = new List<(string Source, List<SkippedSkill> Skills)>();
        foreach (var sk in skipped)
        {
            var src = GetInstallSource(sk);
            var i = grouped.FindIndex(g => g.Source == src);
            if (i >= 0) grouped[i].Skills.Add(sk);
            else grouped.Add((src, [sk]));
        }
        foreach (var (source, skills) in grouped)
        {
            var names = string.Join(", ", skills.Select(x => Sanitize.Metadata(x.Name)));
            Sys.OutLine($"  {Text}•{Reset} {names} {Dim}({skills[0].Reason}){Reset}");
            Sys.OutLine($"    {Dim}To update: {Text}skills add {source} -g -y{Reset}");
        }
    }

    private static List<ProjectSkill> GetProjectSkillsForUpdate(List<string>? filter)
    {
        var output = new List<ProjectSkill>();
        foreach (var (name, entry) in LocalLock.Read().Skills)
        {
            if (!MatchesSkillFilter(name, filter) || entry == null) continue;
            if (S(entry, "sourceType") is "node_modules" or "local") continue;
            output.Add(new ProjectSkill(name, entry));
        }
        return output;
    }

    private static void PromptDeletions(string source, List<string> deleted, bool isGlobal, UpdateOptions o)
    {
        if (deleted.Count == 0) return;
        Sys.OutLine();
        Sys.OutLine($"{Dim}Warning:{Reset} The following skills from {Dim}{source}{Reset} appear to have been deleted upstream:");
        foreach (var d in deleted) Sys.OutLine($"  {Dim}•{Reset} {d}");
        if (o.Yes || o.DryRun || !Sys.StdinIsTty())
        {
            Sys.OutLine($"{Dim}Skipping deletion in non-interactive mode.{Reset}");
            return;
        }
        if (Ui.Confirm("Would you like to remove the local copies of these deleted skills?") != true) return;
        foreach (var d in deleted)
        {
            Sys.OutLine($"{Dim}Removing{Reset} {d}…");
            RemoveCommand.Run([d], new RemoveOptions { Yes = true, Global = isGlobal });
        }
    }

    private static Resolution CheckAndPromptForDeletions(string source, List<string> lockedNames, JsonObject lockSkills, bool isGlobal, UpdateOptions o, List<DiscoveredSkillLocation> discovered)
    {
        var r = SkillRelocation.Resolve(lockedNames, lockSkills, discovered);
        if (r.AmbiguousSkills.Count > 0)
        {
            Sys.OutLine();
            Sys.OutLine($"{Dim}Warning:{Reset} Multiple current paths match these skills from {Dim}{source}{Reset}; skipping them rather than deleting or migrating the wrong skill:");
            foreach (var n in r.AmbiguousSkills) Sys.OutLine($"  {Dim}•{Reset} {Sanitize.Metadata(n)}");
        }
        PromptDeletions(source, r.DeletedSkills, isGlobal, o);
        return new Resolution(r.DeletedSkills, r.ResolvedPaths);
    }

    /// `force` (update `--force`) marks every tracked skill still in the index as
    /// changed without comparing digests.
    private static WellKnownCheck? CheckWellKnown(string baseUrl, List<WellKnownItem> items, bool force)
    {
        if (WellKnown.FetchIndex(baseUrl, true) is not { } index) return null;
        var byName = new Dictionary<string, NormalizedEntry>();
        foreach (var e in index.Entries) byName[e.Name] = e;
        var removed = items.Where(i => !byName.ContainsKey(i.Name)).Select(i => i.Name).ToList();
        var local = items.Select(i => i.Name).ToHashSet();
        var newSkills = index.Entries.Select(e => e.Name).Where(n => !local.Contains(n)).ToList();
        var changed = new List<string>();
        var needsContent = new List<WellKnownItem>();
        foreach (var item in items)
        {
            if (!byName.TryGetValue(item.Name, out var entry)) continue;
            if (entry is V2Entry v2)
            {
                if (force || item.Digest.Length == 0 || v2.Digest != item.Digest) changed.Add(item.Name);
            }
            else if (force)
            {
                changed.Add(item.Name);
            }
            else
            {
                needsContent.Add(item);
            }
        }
        if (needsContent.Count > 0)
        {
            var tracked = needsContent.Select(i => i.Name).ToHashSet();
            var entries = index.Entries.Where(e => tracked.Contains(e.Name)).ToList();
            var skills = Blob.ParallelMap(entries, WellKnown.FetchSkillByEntry).Where(s => s != null).Select(s => s!).ToList();
            if (skills.Count == 0) return null;
            var digests = new Dictionary<string, string>();
            foreach (var s in skills) digests[s.InstallName] = WellKnown.ComputeSkillDigest(s);
            foreach (var item in needsContent)
                if (!(digests.TryGetValue(item.Name, out var d) && item.Digest.Length > 0 && d == item.Digest)) changed.Add(item.Name);
        }
        return new WellKnownCheck(changed.Count > 0 || removed.Count > 0, changed, removed, newSkills);
    }

    private static void PrintNewSkills(string baseUrl, List<string> newSkills, bool isGlobal)
    {
        if (newSkills.Count == 0) return;
        var names = newSkills.Select(Sanitize.Metadata).ToList();
        Sys.OutLine($"  {Dim}{newSkills.Count} new skill(s) available from this source:{Reset} {string.Join(", ", names)}");
        Sys.OutLine($"    {Dim}To install: {Text}skills add {baseUrl} --skill {string.Join(" ", names)}{(isGlobal ? " -g" : "")}{Reset}");
    }

    private static string? CliEntry() => Environment.ProcessPath is { } p && File.Exists(p) ? p : null;

    /// Reinstall via `&lt;self&gt; add ...` with stdin inherited and output captured.
    private static bool SpawnAdd(List<string> args, bool githubPin)
    {
        if (CliEntry() is not { } exe) return false;
        var cmd = Proc.Command(exe, args);
        if (githubPin) cmd = cmd.Env("GH_HOST", "github.com");
        try
        {
            return cmd.OutputInheritStdin().Success;
        }
        catch (ProcException)
        {
            return false;
        }
    }

    private static void AddSubagentArgs(List<string> args, List<string>? subs)
    {
        if (subs is not { Count: > 0 }) return;
        args.Add("--subagent");
        args.AddRange(subs.Select(s => s.Length == 0 ? "root" : s));
    }

    private static WellKnownOutcome ProcessWellKnownUpdates(List<(string BaseUrl, List<WellKnownItem> Items)> groups, bool isGlobal, UpdateOptions o)
    {
        int ok = 0, fail = 0;
        var changedAny = false;
        var pending = new List<(string Name, string Source)>();
        foreach (var (baseUrl, items) in groups)
        {
            Sys.Out($"\r{Dim}Checking skills from source: {baseUrl}{Reset}\x1b[K\n");
            var check = CheckWellKnown(baseUrl, items, o.Force);
            if (check == null)
            {
                Sys.OutLine($"  {Dim}✗ Failed to check skills from {baseUrl}{Reset}");
                continue;
            }
            if (!check.Changed)
            {
                PrintNewSkills(baseUrl, check.NewSkills, isGlobal);
                continue;
            }
            changedAny = true;
            PromptDeletions(baseUrl, check.Removed, isGlobal, o);
            PrintNewSkills(baseUrl, check.NewSkills, isGlobal);
            if (check.ChangedSkills.Count == 0) continue;
            if (o.DryRun)
            {
                pending.AddRange(check.ChangedSkills.Select(n => (n, baseUrl)));
                continue;
            }
            if (CliEntry() == null)
            {
                fail += check.ChangedSkills.Count;
                Sys.OutLine($"  {Dim}✗ CLI entrypoint not found{Reset}");
                continue;
            }
            foreach (var name in check.ChangedSkills)
            {
                var safe = Sanitize.Metadata(name);
                Sys.OutLine($"{Text}Updating {safe}…{Reset}");
                var args = new List<string> { "add", baseUrl, "--skill", name };
                if (!isGlobal) AddSubagentArgs(args, items.FirstOrDefault(i => i.Name == name)?.Subagents);
                if (isGlobal) args.Add("-g");
                args.Add("-y");
                if (SpawnAdd(args, false))
                {
                    ok++;
                    Sys.OutLine($"  {Text}✓{Reset} Updated {safe}");
                }
                else
                {
                    fail++;
                    Sys.OutLine($"  {Dim}✗ Failed to update {safe}{Reset}");
                }
            }
        }
        return new WellKnownOutcome(ok, fail, changedAny, pending);
    }

    /// `--dry-run` result: `Found N <scope> update(s)`, one bullet per update,
    /// and the no-changes note.
    private static void PrintDryRunUpdates(string scope, List<(string Name, string Source)> pending)
    {
        Sys.OutLine($"{Text}Found {pending.Count} {scope} update(s){Reset}");
        Sys.OutLine();
        foreach (var (name, source) in pending) Sys.OutLine($"  • {Sanitize.Metadata(name)} {Dim}({Sanitize.Metadata(source)}){Reset}");
        Sys.OutLine();
        Sys.OutLine($"{Dim}Dry run: no changes made. Run skills update without --dry-run to apply.{Reset}");
    }

    private static List<DiscoveredSkillLocation> DiscoveredLocations(string tempDir) =>
        SkillDiscovery.Discover(tempDir, null, new DiscoverOptions(FullDepth: true, IncludeDuplicateNames: true))
            .Select(sk => new DiscoveredSkillLocation(sk.Name, string.Join("/", NodePath.Join(NodePath.Relative(tempDir, sk.Path), "SKILL.md").Split(NodePath.Sep))))
            .ToList();

    private static void AddToGroup<T>(List<(string Key, List<T> Items)> groups, string key, T item)
    {
        var i = groups.FindIndex(g => g.Key == key);
        if (i >= 0) groups[i].Items.Add(item);
        else groups.Add((key, [item]));
    }

    private static bool IsCheckFailure(Exception e) => e is GitCloneException or DiscoverException or IOException or UnauthorizedAccessException;

    private static (int Ok, int Fail, int Checked) UpdateGlobalSkills(UpdateOptions o)
    {
        var skills = SkillLock.Read().Skills;
        int ok = 0, fail = 0;
        if (skills.Count == 0)
        {
            if (o.Skills == null)
            {
                Sys.OutLine($"{Dim}No global skills tracked in lock file.{Reset}");
                Sys.OutLine($"{Dim}Install skills with{Reset} {Text}skills add <package> -g{Reset}");
            }
            return (0, 0, GhInstalled.ReportGhSkills(true, skills, o.Skills));
        }

        var updates = new List<(string Name, JsonNode Entry)>();
        var skipped = new List<SkippedSkill>();
        var checkable = new List<(string Name, JsonNode Entry)>();
        var wkGroups = new List<(string Key, List<WellKnownItem> Items)>();
        var pinned = new List<(string Name, string Ref)>();
        var unpinned = new List<(string Name, JsonNode Entry)>();

        foreach (var (name, entry) in skills)
        {
            if (!MatchesSkillFilter(name, o.Skills) || entry == null) continue;
            var action = Pinning.GetPinAction(entry, o.Force, o.Unpin);
            if (action.Kind == PinActionKind.Skip)
            {
                pinned.Add((name, action.Ref!));
                continue;
            }
            if (action.Kind == PinActionKind.Unpin)
            {
                unpinned.Add((name, Pinning.UnpinnedEntry(entry)!));
                continue;
            }
            if (S(entry, "sourceType") == "well-known" && NonEmpty(entry, "sourceBaseUrl") is { } b && NonEmpty(entry, "wellKnownDigest") is { } digest)
            {
                AddToGroup(wkGroups, b, new WellKnownItem(name, digest, null));
                continue;
            }
            if (NonEmpty(entry, "skillFolderHash") == null || NonEmpty(entry, "skillPath") == null)
            {
                skipped.Add(new SkippedSkill(name, GetSkipReason(entry), S(entry, "sourceUrl") ?? "", S(entry, "sourceType") ?? "", S(entry, "ref")));
                continue;
            }
            checkable.Add((name, entry));
        }

        var wkCount = wkGroups.Sum(g => g.Items.Count);
        var wk = ProcessWellKnownUpdates(wkGroups, true, o);
        ok += wk.Ok;
        fail += wk.Fail;
        var wkChanged = wk.ChangedAny;

        var bySource = new List<(string Key, List<(string Name, JsonNode Entry)> Items)>();
        foreach (var item in checkable) AddToGroup(bySource, $"{S(item.Entry, "source")}\n{S(item.Entry, "ref")}", item);

        foreach (var (_, items) in bySource)
        {
            var first = items[0].Entry;
            var source = S(first, "source") ?? "";
            var sourceUrl = NonEmpty(first, "sourceUrl") ?? source;
            var firstRef = S(first, "ref");
            Sys.Out($"\r{Dim}Checking skills from source: {source}{Reset}\x1b[K\n");
            var isGithub = S(first, "sourceType") == "github";
            var lockedForSource = skills.Where(kv => S(kv.Value, "source") == source && S(kv.Value, "ref") == firstRef).Select(kv => kv.Key).ToList();

            if (isGithub)
            {
                if (Blob.FetchRepoTree(source, firstRef, true) is { } tree)
                {
                    var blobPaths = tree.Tree.Where(e => e.Kind == "blob").Select(e => e.Path).ToHashSet();
                    var missing = lockedForSource.Any(n => NonEmpty(skills[n], "skillPath") is { } p && !blobPaths.Contains(p));
                    if (!missing)
                    {
                        foreach (var (name, entry) in items)
                        {
                            if (Blob.GetSkillFolderHashFromTree(tree, S(entry, "skillPath") ?? "") is { } latest && (o.Force || latest != S(entry, "skillFolderHash")))
                                updates.Add((name, entry));
                        }
                        continue;
                    }
                    Sys.OutLine($"  {Dim}Skill paths changed; resolving via Git clone{Reset}");
                }
                else
                {
                    Sys.OutLine($"  {Dim}GitHub API unavailable; checking via Git clone{Reset}");
                }
            }

            string? temp = null;
            try
            {
                temp = Git.CloneRepo(sourceUrl, firstRef);
                var locations = DiscoveredLocations(temp);
                var res = CheckAndPromptForDeletions(source, lockedForSource, skills, true, o, locations);
                foreach (var (name, entry) in items)
                {
                    if (res.Deleted.Contains(name) || !res.Resolved.TryGetValue(name, out var sp)) continue;
                    var hash = S(entry, "skillFolderHash") ?? "";
                    var usesTreeHash = isGithub && hash.Length == 40 && hash.All(char.IsAsciiHexDigit);
                    string? latest;
                    if (usesTreeHash)
                    {
                        latest = Git.GetGitTreeHash(temp, sp);
                    }
                    else
                    {
                        try
                        {
                            latest = LocalLock.ComputeSkillFolderHash(NodePath.Join(temp, NodePath.Dirname(sp)));
                        }
                        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
                        {
                            latest = null;
                        }
                    }
                    var relocated = sp != S(entry, "skillPath");
                    if (relocated || o.Force || (latest != null && latest != hash))
                    {
                        var e = entry.DeepClone();
                        e["skillPath"] = sp;
                        updates.Add((name, e));
                    }
                }
            }
            catch (Exception e) when (IsCheckFailure(e))
            {
                Sys.OutLine($"  {Dim}✗ Failed to check skills from {source}{Reset}");
            }
            finally
            {
                Git.TryCleanup(temp);
            }
        }

        if (checkable.Count > 0) Sys.Out("\r\x1b[K");
        var nothingToCheck = checkable.Count == 0 && skipped.Count == 0 && wkCount == 0 && unpinned.Count == 0;
        var checkedCount = nothingToCheck ? pinned.Count : checkable.Count + skipped.Count + wkCount + unpinned.Count + pinned.Count;
        // `--unpin`: pinned skills are reinstalled from their default branch.
        updates.AddRange(unpinned);
        var hasUpdates = updates.Count > 0 || (o.DryRun && wk.Pending.Count > 0);
        if (nothingToCheck)
        {
            if (pinned.Count == 0 && o.Skills == null) Sys.OutLine($"{Dim}No global skills to check.{Reset}");
        }
        else if (!hasUpdates)
        {
            if (!(checkable.Count == 0 && skipped.Count > 0) && !wkChanged) Sys.OutLine($"{Text}✓ All global skills are up to date{Reset}");
        }
        else if (o.DryRun)
        {
            var pending = new List<(string Name, string Source)>(wk.Pending);
            pending.AddRange(updates.Select(u => (u.Name, S(u.Entry, "source") ?? "")));
            PrintDryRunUpdates("global", pending);
        }
        else
        {
            var (a, b) = ReinstallGlobalUpdates(updates);
            ok += a;
            fail += b;
        }
        Pinning.PrintPinnedNotice(pinned);
        checkedCount += GhInstalled.ReportGhSkills(true, skills, o.Skills);
        PrintSkippedSkills(skipped);
        return (ok, fail, checkedCount);
    }

    private static (int Ok, int Fail) ReinstallGlobalUpdates(List<(string Name, JsonNode Entry)> updates)
    {
        int ok = 0, fail = 0;
        Sys.OutLine($"{Text}Found {updates.Count} global update(s){Reset}");
        Sys.OutLine();
        foreach (var (name, entry) in updates)
        {
            var safe = Sanitize.Metadata(name);
            Sys.OutLine($"{Text}Updating {safe}…{Reset}");
            // `--force` on a pinned skill: reinstall at its ref and keep it pinned.
            var pin = Pinning.PinnedRef(entry);
            var useEntry = UpdateSourceEntry.FromJson(entry);
            if (pin != null) useEntry = useEntry with { Ref = null };
            if (UpdateSource.BuildUpdateInstallSource(useEntry) is not { } installUrl)
            {
                fail++;
                Sys.OutLine($"  {Dim}✗ Cannot update {safe}: lock file is missing sourceUrl for this generic Git source{Reset}");
                continue;
            }
            if (CliEntry() == null)
            {
                fail++;
                Sys.OutLine($"  {Dim}✗ Failed to update {safe}: CLI entrypoint not found{Reset}");
                continue;
            }
            var args = new List<string> { "add", installUrl, "--skill", name };
            if (pin != null)
            {
                args.Add("--pin");
                args.Add(pin);
            }
            if (UpdateSource.ShouldUseFullDepthForUpdate(useEntry)) args.Add("--full-depth");
            args.Add("-g");
            args.Add("-y");
            if (SpawnAdd(args, useEntry.SourceType == "github"))
            {
                ok++;
                Sys.OutLine($"  {Text}✓{Reset} Updated {safe}");
            }
            else
            {
                fail++;
                Sys.OutLine($"  {Dim}✗ Failed to update {safe}{Reset}");
            }
        }
        return (ok, fail);
    }

    private static void PrintLegacyProjectSkills(List<ProjectSkill> legacy)
    {
        if (legacy.Count == 0) return;
        Sys.OutLine();
        Sys.OutLine($"{Dim}{legacy.Count} project skill(s) cannot be updated automatically (installed before skillPath tracking):{Reset}");
        foreach (var sk in legacy)
        {
            var reinstall = UpdateSource.BuildLocalUpdateSource(UpdateSourceEntry.FromJson(sk.Entry));
            Sys.OutLine($"  {Text}•{Reset} {Sanitize.Metadata(sk.Name)}");
            Sys.OutLine(reinstall != null
                ? $"    {Dim}To refresh: {Text}skills add {reinstall} -y{Reset}"
                : $"    {Dim}To refresh: reinstall using the original full Git URL; this lock entry only has an ambiguous shorthand.{Reset}");
        }
    }

    private static string SourceKey(JsonNode? e) => NonEmpty(e, "sourceUrl") ?? S(e, "source") ?? "";

    private static (int Ok, int Fail, int Checked) UpdateProjectSkills(UpdateOptions o)
    {
        var all = GetProjectSkillsForUpdate(o.Skills);
        var localLock = LocalLock.Read();
        int ok = 0, fail = 0;
        if (all.Count == 0)
        {
            if (o.Skills == null)
            {
                Sys.OutLine($"{Dim}No project skills to update.{Reset}");
                Sys.OutLine($"{Dim}Install project skills with{Reset} {Text}skills add <package>{Reset}");
            }
            return (0, 0, GhInstalled.ReportGhSkills(false, localLock.Skills, o.Skills));
        }
        var total = all.Count;

        // Pinned skills are skipped (listed in a notice) unless --force/--unpin;
        // --unpin reinstalls them from their default branch.
        var pinned = new List<(string Name, string Ref)>();
        var project = new List<ProjectSkill>();
        var lockSkills = (JsonObject)localLock.Skills.DeepClone();
        foreach (var sk in all)
        {
            var action = Pinning.GetPinAction(sk.Entry, o.Force, o.Unpin);
            switch (action.Kind)
            {
                case PinActionKind.Skip:
                    pinned.Add((sk.Name, action.Ref!));
                    break;
                case PinActionKind.Unpin:
                {
                    var entry = Pinning.UnpinnedEntry(sk.Entry)!;
                    lockSkills[sk.Name] = entry.DeepClone();
                    project.Add(sk with { Entry = entry });
                    break;
                }
                default:
                    project.Add(sk);
                    break;
            }
        }
        if (project.Count == 0)
        {
            Pinning.PrintPinnedNotice(pinned);
            var gh = GhInstalled.ReportGhSkills(false, localLock.Skills, o.Skills);
            return (0, 0, total + gh);
        }

        var wkGroups = new List<(string Key, List<WellKnownItem> Items)>();
        var nonWk = new List<ProjectSkill>();
        foreach (var sk in project)
        {
            if (S(sk.Entry, "sourceType") == "well-known" && NonEmpty(sk.Entry, "sourceUrl") is { } url && NonEmpty(sk.Entry, "wellKnownDigest") is { } digest)
            {
                AddToGroup(wkGroups, url, new WellKnownItem(sk.Name, digest, Json.StringArray(sk.Entry, "subagents")));
                continue;
            }
            nonWk.Add(sk);
        }
        var wkCount = wkGroups.Sum(g => g.Items.Count);
        var updatable = nonWk.Where(x => NonEmpty(x.Entry, "skillPath") != null).ToList();
        var legacy = nonWk.Where(x => NonEmpty(x.Entry, "skillPath") == null).ToList();

        if (updatable.Count == 0 && wkCount == 0)
        {
            Sys.OutLine($"{Dim}No project skills can be updated in place.{Reset}");
            Pinning.PrintPinnedNotice(pinned);
            var gh = GhInstalled.ReportGhSkills(false, localLock.Skills, o.Skills);
            PrintLegacyProjectSkills(legacy);
            return (ok, fail, total + gh);
        }

        if (!o.DryRun)
        {
            var cwd = Sys.Cwd();
            var targets = new List<string>();
            var hasUniversal = false;
            foreach (var a in Agents.List)
            {
                if (Agents.IsUniversalAgent(a.Name))
                {
                    if (!hasUniversal && Fs.Exists(NodePath.Join(cwd, ".agents"))) hasUniversal = true;
                }
                else if (Fs.Exists(NodePath.Join(cwd, a.SkillsDir.Split('/')[0])))
                {
                    targets.Add(a.DisplayName);
                }
            }
            var parts = new List<string>();
            if (hasUniversal) parts.Add("Universal");
            parts.AddRange(targets);
            if (parts.Count > 0) Sys.OutLine($"{Text}Updating for: {string.Join(", ", parts)}{Reset}");
            Sys.OutLine($"{Text}Refreshing {updatable.Count + wkCount} skill(s)…{Reset}");
            Sys.OutLine();
        }

        var wk = ProcessWellKnownUpdates(wkGroups, false, o);
        ok += wk.Ok;
        fail += wk.Fail;
        var pendingUpdates = new List<(string Name, string Source)>(wk.Pending);

        var bySource = new List<(string Key, List<ProjectSkill> Items)>();
        foreach (var sk in updatable) AddToGroup(bySource, $"{SourceKey(sk.Entry)}\n{S(sk.Entry, "ref")}", sk);

        if (updatable.Count > 0 && CliEntry() == null)
        {
            Sys.OutLine($"{Dim}✗ CLI entrypoint not found{Reset}");
            return (ok, fail + updatable.Count, total);
        }

        foreach (var (_, group) in bySource)
        {
            var first = group[0].Entry;
            var source = SourceKey(first);
            var cloneSource = UpdateSource.BuildLocalCloneSource(UpdateSourceEntry.FromJson(first));
            var r = S(first, "ref");
            var lockedForSource = lockSkills.Where(kv => SourceKey(kv.Value) == source && S(kv.Value, "ref") == r).Select(kv => kv.Key).ToList();

            if (cloneSource == null)
            {
                fail += group.Count;
                Sys.OutLine($"{Dim}✗ Cannot update {source}: skills-lock.json is missing sourceUrl for this generic Git source{Reset}");
                continue;
            }

            Resolution res;
            string? temp = null;
            try
            {
                temp = Git.CloneRepo(cloneSource, r);
                res = CheckAndPromptForDeletions(source, lockedForSource, lockSkills, false, o, DiscoveredLocations(temp));
            }
            catch (Exception e) when (IsCheckFailure(e))
            {
                Git.TryCleanup(temp);
                Sys.OutLine($"{Dim}✗ Failed to check for deleted skills from {source}{Reset}");
                fail += group.Count;
                continue;
            }
            Git.TryCleanup(temp);

            foreach (var sk in group.Where(x => !res.Deleted.Contains(x.Name)))
            {
                var safe = Sanitize.Metadata(sk.Name);
                if (!res.Resolved.TryGetValue(sk.Name, out var resolved)) continue;
                if (o.DryRun)
                {
                    pendingUpdates.Add((sk.Name, S(sk.Entry, "source") ?? ""));
                    continue;
                }
                var entry = sk.Entry.DeepClone();
                entry["skillPath"] = resolved;
                Sys.OutLine($"{Text}Updating {safe}…{Reset}");
                // `--force` on a pinned skill: reinstall at its ref and keep it pinned.
                var pin = Pinning.PinnedRef(entry);
                var useEntry = UpdateSourceEntry.FromJson(entry);
                if (pin != null) useEntry = useEntry with { Ref = null };
                if (UpdateSource.BuildLocalUpdateSource(useEntry) is not { } installUrl)
                {
                    fail++;
                    Sys.OutLine($"  {Dim}✗ Cannot update {safe}: skills-lock.json is missing sourceUrl for this generic Git source{Reset}");
                    continue;
                }
                var args = new List<string> { "add", installUrl, "--skill", sk.Name };
                if (pin != null)
                {
                    args.Add("--pin");
                    args.Add(pin);
                }
                AddSubagentArgs(args, Json.StringArray(sk.Entry, "subagents"));
                if (UpdateSource.ShouldUseFullDepthForUpdate(useEntry)) args.Add("--full-depth");
                args.Add("-y");
                if (SpawnAdd(args, useEntry.SourceType == "github"))
                {
                    ok++;
                    Sys.OutLine($"  {Text}✓{Reset} Updated {safe}");
                }
                else
                {
                    fail++;
                    Sys.OutLine($"  {Dim}✗ Failed to update {safe}{Reset}");
                }
            }
        }

        if (o.DryRun)
        {
            if (pendingUpdates.Count == 0) Sys.OutLine($"{Text}✓ All project skills are up to date{Reset}");
            else PrintDryRunUpdates("project", pendingUpdates);
        }
        Pinning.PrintPinnedNotice(pinned);
        var ghCount = GhInstalled.ReportGhSkills(false, localLock.Skills, o.Skills);
        PrintLegacyProjectSkills(legacy);
        return (ok, fail, total + ghCount);
    }

    public static void Run(IReadOnlyList<string> args)
    {
        var o = ParseOptions(args);
        var scope = ResolveScope(o);
        Sys.OutLine(o.Skills != null ? $"{Text}Updating {string.Join(", ", o.Skills)}…{Reset}" : $"{Text}Checking for skill updates…{Reset}");
        Sys.OutLine();

        int totalOk = 0, totalFail = 0, totalFound = 0;
        var headers = scope == UpdateScope.Both && o.Skills == null;
        if (scope is UpdateScope.Global or UpdateScope.Both)
        {
            if (headers) Sys.OutLine($"{Bold}Global Skills{Reset}");
            var (a, b, c) = UpdateGlobalSkills(o);
            totalOk += a;
            totalFail += b;
            totalFound += c;
            if (headers) Sys.OutLine();
        }
        if (scope is UpdateScope.Project or UpdateScope.Both)
        {
            if (headers) Sys.OutLine($"{Bold}Project Skills{Reset}");
            var (a, b, c) = UpdateProjectSkills(o);
            totalOk += a;
            totalFail += b;
            totalFound += c;
        }

        if (o.Skills != null && totalFound == 0) Sys.OutLine($"{Dim}No installed skills found matching: {string.Join(", ", o.Skills)}{Reset}");
        Sys.OutLine();
        if (totalOk > 0) Sys.OutLine($"{Text}✓ Updated {totalOk} skill(s){Reset}");
        if (totalFail > 0)
        {
            Sys.OutLine($"{Dim}Failed to update {totalFail} skill(s){Reset}");
            Sys.ExitCode = 1;
        }
        Telemetry.Track(
            ("event", "update"),
            ("scope", ScopeName(scope)),
            ("skillCount", (totalOk + totalFail).ToString()),
            ("successCount", totalOk.ToString()),
            ("failCount", totalFail.ToString()));
        Sys.OutLine();
    }
}

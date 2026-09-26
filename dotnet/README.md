# skills (C# port)

A self-contained, single-executable C# (.NET 10) port of the `skills` CLI. The
original TypeScript CLI, [vercel-labs/skills](https://github.com/vercel-labs/skills),
is the reference implementation. This port follows its behavior command by
command, like the [Rust port](../rust), and is checked against it by the same
side-by-side parity harness.

The published executable bundles the .NET runtime, so it has no runtime
dependencies (no Node.js, no installed .NET). Like the TypeScript CLI, it runs
only these external tools: `git` for cloning, and optionally `gh` (GitHub CLI
auth fallbacks), `ntn` (Notion), and `claude`/`codex`/`sarvam-code` for
`skills use --agent`.

## Library (NuGet)

The core is also a NuGet package,
[`Devolutions.AgentSkills`](lib/Devolutions.AgentSkills/README.md), for
applications that manage skills themselves (for example UniGetUI). It exposes
the CLI's operations as a UI-free API on `SkillsManager`:

- `GetAgents` and `GetInstalledSkills`
- `Search`, which queries skills.sh
- `GetAvailableSkills`, the equivalent of `add --list`
- `Install`, `Remove`, `CheckForUpdates` and `Update`

Each has an `Async` variant, and the long-running ones take progress and
cancellation arguments. The library writes the same directories and lock files
as the CLI, is trim- and NativeAOT-safe, and never prompts, prints or exits the
process. The CLI is built on the same core.

```bash
dotnet pack lib/Devolutions.AgentSkills -c Release -o nupkg   # → nupkg/Devolutions.AgentSkills.<version>.nupkg
```

## Build

```bash
cd dotnet
dotnet build                                                   # debug build (bin/)
dotnet test                                                    # unit tests
dotnet publish src/Skills.csproj -c Release -r win-x64 -o publish   # → publish/skills.exe
```

This needs the .NET 10 SDK. Use any runtime identifier for `-r`, for example
`win-x64`, `win-arm64`, `linux-x64`, `linux-arm64`, `osx-x64` or `osx-arm64`.
Cross-publishing works from any host and needs no native toolchain. A publish
with `-r` produces a trimmed, compressed, ReadyToRun single file of about
17 MB. On win-x64, `skills list` takes about 130 to 150 ms. The Rust binary
takes about 35 ms and the built npm CLI about 220 ms. Pass
`-p:EnableCompressionInSingleFile=false` to get about 100 ms at about 27 MB.

### NativeAOT

The port is NativeAOT-compatible. `IsAotCompatible` runs the trim, AOT and
single-file analyzers on every build, and warnings are errors, so code that
would break under AOT fails `dotnet build`. To publish a native executable
(about 9 MB, about 60 ms for `skills list` on win-x64):

```bash
dotnet publish src/Skills.csproj -c Release -r win-x64 -p:PublishAot=true -o publish-aot
```

NativeAOT needs the platform toolchain: the Visual Studio "Desktop development
with C++" workload on Windows, clang or gcc (plus zlib) on Linux, and Xcode on
macOS. It builds only for the host OS. If the ILCompiler can't locate MSVC
through `vswhere`, put `link.exe` on `PATH`, set `LIB` to the MSVC and Windows
SDK library folders, and pass `-p:IlcUseEnvironmentalTools=true`. The win-x64
NativeAOT build passes all 89 parity cases.

Run `skills update` from a published executable or the `bin/` apphost
(`skills.exe`), not through `dotnet skills.dll`. Update reinstalls changed
skills by running the current process's executable with `add`.

## Usage

The commands are the same as the npm CLI's:

```bash
skills add vercel-labs/agent-skills
skills add ./my-skills -y -a claude-code cursor
skills add owner/repo --json -y
skills use vercel-labs/skills@find-skills
skills list --json
skills remove my-skill -y
skills update -g -y
skills find typescript
skills init my-skill
skills experimental_install
skills experimental_sync -y
```

`skills --help` prints the full reference.

## Layout

The code is split into two projects:

- `lib/Devolutions.AgentSkills/` is the library. It holds the core: agents,
  sources, discovery, installer, lock files and HTTP/Git. The public API is in
  `Api/`.
- `src/` is the CLI (assembly `skills`). It holds the terminal UI and the
  command flows, and uses the library's internals.

In the table below, `†` marks files in the library. `InstallRecords.cs`,
`Removal.cs`, `UpdateChecks.cs` and `SearchApi.cs` hold the parts of
`add.ts`, `remove.ts`, `update.ts` and `find.ts` that the CLI and the public
API share. `Pinning.cs` (pinned lock entries) is also in the library, so API
update checks leave pinned skills alone, as `skills update` does.

Each file ports the TypeScript module (and the Rust module) of the same name
in the reference CLI's [`src/`](https://github.com/vercel-labs/skills/tree/main/src):

| TypeScript (`src/`)                          | C#                                                 |
| -------------------------------------------- | -------------------------------------------------- |
| `cli.ts`                                     | `Main.cs`                                          |
| `add.ts`                                     | `AddHelpers.cs`, `AddWellKnown.cs`, `AddRun.cs`    |
| `agents.ts`, `types.ts`                      | `Agents.cs`†, `Types.cs`†                          |
| `archive.ts`                                 | `Archive.cs`†                                      |
| `blob.ts`                                    | `Blob.cs`†                                         |
| `detect-agent.ts` (+ `@vercel/detect-agent`) | `DetectAgent.cs`                                   |
| `download-source.ts`                         | `DownloadSource.cs`†                               |
| `find.ts`                                    | `Find.cs`                                          |
| `frontmatter.ts` (+ `yaml`)                  | `Frontmatter.cs`†                                  |
| `git.ts` (+ `simple-git`)                    | `Git.cs`†                                          |
| `github-host.ts`                             | `GitHubHost.cs`†                                   |
| `install.ts`                                 | `InstallLock.cs`                                   |
| `installer.ts`                               | `Installer.cs`†                                    |
| `list.ts`                                    | `List.cs`                                          |
| `local-lock.ts`                              | `LocalLock.cs`†                                    |
| `notion-test.ts`                             | `Notion.cs`                                        |
| `plugin-manifest.ts`                         | `PluginManifest.cs`†                               |
| `prompts/search-multiselect.ts`              | `SearchMultiselect.cs`                             |
| `providers/wellknown.ts`                     | `WellKnown.cs`†                                    |
| `remove.ts`                                  | `Remove.cs`                                        |
| `sanitize.ts`                                | `Sanitize.cs`†                                     |
| `skill-lock.ts`                              | `SkillLock.cs`†                                    |
| `skill-relocation.ts`, `update-source.ts`    | `UpdateSource.cs`†                                 |
| `skills.ts`                                  | `Skills.cs`†                                       |
| `source-parser.ts`                           | `SourceParser.cs`†                                 |
| `sync.ts`                                    | `Sync.cs`                                          |
| `telemetry.ts`                               | `Telemetry.cs`†                                    |
| `update.ts`                                  | `Update.cs`                                        |
| `use.ts`                                     | `Use.cs`                                           |

Supporting files stand in for Node built-ins and npm packages:

- `NodePath.cs`: Node's `path` semantics (`join`, `resolve`, `normalize`,
  `relative`, posix and win32). The TS code checks for path traversal by
  comparing normalized path strings by prefix, so the port reproduces those
  rules exactly instead of using `System.IO.Path`.
- `Sys.cs` (library): `os.homedir()` and `os.tmpdir()` (libuv rules), the
  environment, and the per-call context that gives each public API call its
  own project and home directories, warning sink and cancellation.
- `Term.cs`: `process.exit` semantics with exit hooks, Windows console VT
  mode, and stdout routing. `add --json` sends all human-readable output to
  stderr, so stdout carries exactly one JSON value.
- `Fs.cs`: `fs` behavior the CLI depends on. On Windows, directory links are
  created as junctions (`FSCTL_SET_REPARSE_POINT`), as Node does, and
  removing a link never touches its target.
- `Json.cs`: `JSON.parse` and `JSON.stringify`, byte for byte (number
  formatting, escaping), over `System.Text.Json.Nodes`, plus JS object key
  order (integer-like keys first).
- `Color.cs`: `picocolors` (including its nested-style handling and its
  "always color on Windows" detection) and `util.styleText` as clack uses it.
- `Ui.cs`: the `@clack/prompts` subset (intro, outro, log, note, spinner,
  select, confirm, multiselect), including `wrap-ansi` wrapping and clack's
  "Canceled" handler for an exit during a spinner.
- `Collate.cs`: an approximation of `localeCompare` (ICU root collation). It
  is needed because the TS CLI sorts files with `localeCompare` before hashing
  them into the `computedHash` in `skills-lock.json`.
- `Http.cs`, `Proc.cs`, `WebUrl.cs`: `fetch` (no proxy, as Node's), `execFile`
  and `spawn` with timeouts and output caps, and `encodeURIComponent`,
  `URLSearchParams` and WHATWG URL parsing.

## Tests

This folder is self-contained. It builds, tests and publishes without the
TypeScript CLI or Node.js. Only the parity harness needs them.

- `tests/`: 111 xUnit tests. Most are ported from the Rust unit tests, including
  a check that `Program.Version` matches the csproj `<Version>`. The
  `SkillsManagerTests` cover the public API against a sandbox home and project,
  using local sources only.
- [`parity/parity.ps1`](parity/parity.ps1) (PowerShell 7) runs the shared
  harness in [`../rust/parity/parity.ps1`](../rust/parity/parity.ps1) against
  `publish/skills(.exe)`. The harness runs the reference TypeScript CLI and the
  port in fresh, identical sandboxes. It compares exit codes, stdout, stderr and
  the resulting file trees, including symlinks, junctions and lock-file
  contents. It needs Node.js 22.18+ and a built checkout of
  [vercel-labs/skills](https://github.com/vercel-labs/skills), passed with
  `-Reference` or the `SKILLS_REFERENCE` environment variable.

```bash
git clone https://github.com/vercel-labs/skills ../skills
pnpm --dir ../skills install && pnpm --dir ../skills build   # update cases need dist/
dotnet publish dotnet/src/Skills.csproj -c Release -r win-x64 -o dotnet/publish
pwsh dotnet/parity/parity.ps1 -Reference ../skills           # offline cases
pwsh dotnet/parity/parity.ps1 -Reference ../skills -Network  # + GitHub / skills.sh cases
pwsh dotnet/parity/parity.ps1 -Reference ../skills -Filter add -ShowOutput
```

Current status: all 89 cases pass (78 offline, 11 network) on Windows against
vercel-labs/skills 1.7.0. The
network cases use the anonymous GitHub API (60 requests per hour). If a
network case fails, rerun it before suspecting a regression. The linux-x64
build was smoke-tested under WSL (Ubuntu 24.04): `init`, `add`, `list` and
`remove`. The osx-arm64 build is compiled but has not been run.
Intentional and known differences are listed in [PARITY.md](PARITY.md).

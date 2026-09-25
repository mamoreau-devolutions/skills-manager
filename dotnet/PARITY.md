# Parity notes

Differences between the C# port and the reference TypeScript CLI
([vercel-labs/skills](https://github.com/vercel-labs/skills)), grouped by
whether they are intentional. Everything not listed here is meant to behave
identically. The shared harness (`parity/parity.ps1`, which runs
`../rust/parity/parity.ps1`) enforces that for the cases it covers. The C#
port was translated from the Rust port, so it has the same deliberate
differences. The entries that are specific to .NET are marked.

## Intentional

- **`experimental_sync` warnings.** For every package in `node_modules`, the
  TS CLI calls `parseSkillMd` on `<pkg>/SKILL.md` and on every candidate
  subdirectory. So it prints `⚠ Skipped … failed to read file: ENOENT` for each
  package that has no root skill. The port checks that the file exists first,
  and only warns about SKILL.md files that exist but are invalid.
- **`experimental_sync` ordering.** TS discovers node_modules skills
  concurrently and reports them in the order the promises complete, which
  changes from run to run. The port uses directory order, so its output is
  deterministic.
- **Blob fast path and invalid remote frontmatter.** In TS, a YAML error in a
  remote SKILL.md during the skills.sh snapshot fast path aborts the install.
  The port falls back to the git clone path, where the broken skill is skipped
  with a warning, like any local one.
- **Spinner frames without a TTY.** clack keeps animating when stdout is a pipe,
  and writes frames and cursor-control sequences into logs. The port animates
  only on a terminal. The start and stop lines (and the cursor hide and show
  codes) still match.
- **`update` re-invocation.** Changed skills are reinstalled by running this
  executable's own `add` command (`Environment.ProcessPath`), not
  `node bin/cli.mjs add`. *(.NET)* Under `dotnet skills.dll` the process path is
  the `dotnet` host, so updates must run from the apphost or published
  executable.
- **Standalone command hints.** The port is its own executable, so help,
  tips and error hints say `skills …` where the npm CLI says `npx skills …`.
  The harness maps the TS wording before comparing.
- **User agent.** HTTP requests send `skills-cli/<version>` instead of
  Node's `node`. GitHub tree requests already set `skills-cli` in both.
- **One copy per install target directory.** TS cleans and re-copies the
  shared `.agents/skills/<skill>` directory once per target agent, about 20
  times for a default install. The ports copy each skill once per run, copy
  files in parallel, and produce the same tree. A 155 MB, 2,900-file skill
  installs in about 17 s instead of about 110 s (TS) on Windows.

## Unavoidable or approximated

- **YAML parse error text.** *(.NET)* The port parses frontmatter with
  YamlDotNet's syntax tree and resolves scalars with the YAML 1.2 core schema,
  as the `yaml` npm package does. Invalid frontmatter is rejected by both, but
  the message after `YAML parse error:` is different (TS also prints a source
  code frame). Raw control characters inside values are accepted, as the
  `yaml` package accepts them.
- **Clone failures.** The "Failed to clone" message drops simple-git's trailing
  blank lines. The clone timeout (`SKILLS_CLONE_TIMEOUT_MS`) limits the total
  run time, while simple-git's `block` timeout measures time without output.
  Without a TTY, git prints nothing while it clones, so the two rarely differ.
- **Malformed input handled more leniently.** A non-string entry in a plugin
  manifest's `skills` array is skipped, where TS drops the rest of the
  manifest. An Eve SKILL.md whose frontmatter cannot be parsed is written
  unchanged, where TS fails that install target. Both cases need input that
  discovery already rejects in normal flows.
- **OS error text.** *(.NET)* Messages that embed a Node error (for example
  `ENOENT: no such file or directory, open '…'`) show the .NET exception
  message instead. Node's wording is kept for a missing `git` executable and
  for files that disappear during a copy.
- **`localeCompare` ordering.** Sorting that feeds hashes (`computedHash`,
  snapshot hashes) and a few prompts uses an approximation of ICU root
  collation. The port runs with `InvariantGlobalization`, so it does not depend
  on the host's ICU. The approximation matches ICU for ASCII file names:
  punctuation sorts before digits, digits before letters, letters compare
  case-insensitively first, and lowercase sorts before uppercase. Names with
  non-ASCII characters may sort differently. That would give a different
  `computedHash` than the TS CLI for the same folder.
- **Eve frontmatter re-serialization.** Eve installs rewrite the SKILL.md
  frontmatter. The port's YAML emitter follows `yaml.stringify` defaults for
  common shapes (plain and quoted scalars, nested maps, block sequences,
  folding at 80 columns), but it may format unusual values differently.
- **Interactive prompts.** *(.NET)* Prompts are drawn with `System.Console` in
  the style of clack 1.x (same symbols, colors and key bindings), but they are
  not byte-identical. The automated harness does not cover them because it has
  no TTY. When stdin is not a terminal, a prompt renders its cancelled frame
  and cancels, as clack does on EOF. Keystrokes piped to stdin (for example
  `yes | skills add …`) are not interpreted.
- **Startup time.** *(.NET)* On win-x64, `skills list` takes about 130 to
  150 ms with the ReadyToRun single file and about 60 ms with a NativeAOT
  build. It takes about 35 ms with the Rust binary and about 220 ms with the
  built npm CLI (`node bin/cli.mjs`).

## Extensions

Features the ports add on top of the reference CLI (`preview`, `validate`,
`add --pin`, `update --dry-run/--force/--unpin`, `gh skill` interop) are
described in [`docs/EXTENSIONS.md`](../docs/EXTENSIONS.md). They are additive:
without them, output matches the reference CLI except for the help screens,
which list the new commands and flags. The parity harness skips the extension
cases and the help screens in reference mode; `parity.ps1 -Lockstep` checks
them between the Rust and C# ports.

## Not ported (unused by the CLI)

- The provider registry (`providers/registry.ts`),
  `WellKnownProvider.fetchSkill`, `toRawUrl` and `hasSkillsIndex`, and
  `installRemoteSkillForAgent`. No command path reaches them.
- Build and dev scripts (`scripts/*.ts`: agent validation, README sync, license
  generation).

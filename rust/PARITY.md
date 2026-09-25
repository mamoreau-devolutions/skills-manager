# Parity notes

Differences between the Rust port and the reference TypeScript CLI
([vercel-labs/skills](https://github.com/vercel-labs/skills)), grouped by
whether they are intentional. Everything not listed here is meant to behave
identically; the harness in `parity/` enforces that for the cases it covers.

## Intentional

- **`experimental_sync` warnings.** The TS CLI calls `parseSkillMd` on
  `<pkg>/SKILL.md` for every package in `node_modules` and on every candidate
  subdirectory, so it prints `⚠ Skipped … failed to read file: ENOENT` for each
  package that has no root skill. The port checks for the file first and only
  warns about SKILL.md files that exist but are invalid.
- **`experimental_sync` ordering.** TS discovers node_modules skills
  concurrently and reports them in promise-completion order, which varies from
  run to run. The port uses directory order, so output is deterministic.
- **Blob fast path and invalid remote frontmatter.** In TS, a YAML error in a
  remote SKILL.md during the skills.sh snapshot fast path aborts the install.
  The port falls back to the git clone path, where the broken skill is skipped
  with a warning like any local one.
- **Spinner frames without a TTY.** clack keeps animating when stdout is a pipe,
  writing frames and cursor-control sequences into logs. The port animates only
  on a terminal; the start/stop lines (and cursor hide/show codes) still match.
- **`update` re-invocation.** Changed skills are reinstalled by running this
  executable's own `add` command, not `node bin/cli.mjs add`.
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

- **YAML parse error text.** The port uses `serde_yaml`, not the `yaml` npm
  package. Invalid frontmatter is rejected in both, but the message after
  `YAML parse error:` differs (TS also prints a source code frame). Raw control
  characters inside values, which libyaml would reject, are accepted as the
  `yaml` package does.
- **Clone failures.** The "Failed to clone" message drops simple-git's trailing
  blank lines. The clone timeout (`SKILLS_CLONE_TIMEOUT_MS`) caps total runtime,
  where simple-git's `block` timeout measures time without output; git prints
  nothing while cloning without a TTY, so the two rarely differ.
- **Malformed input handled more leniently.** A non-string entry in a plugin
  manifest's `skills` array is skipped (TS drops the rest of the manifest), and
  an Eve SKILL.md whose frontmatter cannot be parsed is written unchanged (TS
  fails that install target). Both require input that discovery already rejects
  in normal flows.
- **OS error text.** Messages that embed a Node error (for example
  `ENOENT: no such file or directory, open '…'`) show the Rust/OS error text
  instead.
- **`localeCompare` ordering.** Sorting that feeds hashes (`computedHash`,
  snapshot hashes) and a few prompts uses an ICU-root approximation. It matches
  ICU for ASCII file names (punctuation < digits < letters, case-insensitive
  first, lowercase before uppercase). Names with non-ASCII characters may sort
  differently, which would produce a different `computedHash` than the TS CLI
  for the same folder.
- **Eve frontmatter re-serialization.** Eve installs rewrite SKILL.md
  frontmatter. The port's YAML emitter follows `yaml.stringify` defaults for
  common shapes (plain/quoted scalars, nested maps, block sequences, 80-column
  folding) but may format unusual values differently.
- **Interactive prompts.** Prompts are drawn with crossterm in clack's 1.x
  style (symbols, colors, key bindings), but they are not byte-identical and the
  automated harness doesn't cover them, since it has no TTY. When stdin is not a
  terminal, a prompt renders its cancelled frame and cancels, like clack on EOF.
  Piped keystrokes (for example `yes | skills add …`) are not interpreted.

## Not ported (unused by the CLI)

- The provider registry (`providers/registry.ts`) and
  `WellKnownProvider.fetchSkill`/`toRawUrl`/`hasSkillsIndex`, plus
  `installRemoteSkillForAgent`. No command path reaches them.
- Build/dev scripts (`scripts/*.ts`: agent validation, README sync, license
  generation).

## Audit

An audit read every case in the TypeScript test suite (about 870 `it`/`test`
cases and `it.each` rows across 61 files) against the Rust source, verifying
most of them by running both CLIs side by side or with throwaway Rust tests.
It found four real gaps, all fixed and now covered by parity cases:

- `remove` could not delete agent links on Windows (junctions were removed
  with `remove_file`), leaving dangling links (`remove-linked*`).
- A well-known `index.json` with a UTF-8 BOM was rejected (`wk-bom-index`).
- Frontmatter containing raw control characters made the skill invalid
  (`add-control-chars`, `use-control-chars`).
- Lock files did not use JS object key order for integer-like skill names
  (`sync-numeric-names`).

It also caught `use --agent` starting the agent without a console on Windows,
and a download size check that ran before the HTTP status check. Both are fixed.

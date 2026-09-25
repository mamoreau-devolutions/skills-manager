# skills (Rust port)

A self-contained, single-binary Rust port of the `skills` CLI. The original
TypeScript CLI, [vercel-labs/skills](https://github.com/vercel-labs/skills), is
the reference implementation. This port tracks its behavior command by command
and is checked against it by a side-by-side parity harness.

The release binary has no runtime dependencies (no Node.js, no `node_modules`).
It shells out only to the same external tools the TypeScript CLI uses: `git`
for cloning, and optionally `gh` (GitHub CLI auth fallbacks), `ntn` (Notion),
and `claude`/`codex`/`sarvam-code` for `skills use --agent`.

## Build

```bash
cd rust
cargo build --release          # → target/release/skills(.exe)
cargo test                     # unit tests
```

Rust 1.80+ is required (developed with 1.98).

This folder is self-contained. It builds and tests without the TypeScript CLI
or Node.js, and only the parity harness needs them. The version comes from
`Cargo.toml`.

## Usage

Identical to the npm CLI:

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

Each module ports the TypeScript file of the same name in the reference CLI's
[`src/`](https://github.com/vercel-labs/skills/tree/main/src):

| TypeScript (`src/`)             | Rust (`src/`)             |
| ------------------------------- | ------------------------- |
| `cli.ts`                        | `main.rs`                 |
| `add.ts`                        | `add.rs`                  |
| `agents.ts`, `types.ts`         | `agents.rs`, `types.rs`   |
| `archive.ts`                    | `archive.rs`              |
| `blob.ts`                       | `blob.rs`                 |
| `detect-agent.ts` (+ `@vercel/detect-agent`) | `detect_agent.rs` |
| `download-source.ts`            | `download_source.rs`      |
| `find.ts`                       | `find.rs`                 |
| `frontmatter.ts` (+ `yaml`)     | `frontmatter.rs`          |
| `git.ts` (+ `simple-git`)       | `git.rs`                  |
| `github-host.ts`                | `github_host.rs`          |
| `install.ts`                    | `install_lock.rs`         |
| `installer.ts`                  | `installer.rs`            |
| `list.ts`                       | `list.rs`                 |
| `local-lock.ts`                 | `local_lock.rs`           |
| `notion-test.ts`                | `notion.rs`               |
| `plugin-manifest.ts`            | `plugin_manifest.rs`      |
| `prompts/search-multiselect.ts` | `search_multiselect.rs`   |
| `providers/wellknown.ts`        | `wellknown.rs`            |
| `remove.ts`                     | `remove.rs`               |
| `sanitize.ts`                   | `sanitize.rs`             |
| `skill-lock.ts`                 | `skill_lock.rs`           |
| `skill-relocation.ts`           | `skill_relocation.rs`     |
| `skills.ts`                     | `skills.rs`               |
| `source-parser.ts`              | `source_parser.rs`        |
| `sync.ts`                       | `sync.rs`                 |
| `telemetry.ts`                  | `telemetry.rs`            |
| `update.ts`, `update-source.ts` | `update.rs`, `update_source.rs` |
| `use.ts`                        | `use_cmd.rs`              |

Supporting modules replace Node built-ins and npm packages:

- `paths.rs` — Node's `path` semantics (`join`/`resolve`/`normalize`/`relative`,
  posix and win32). The TS code does path-traversal checks as string prefix
  comparisons on normalized paths, so this is reproduced exactly rather than
  using `std::path`.
- `sys.rs` — `os.homedir()`/`os.tmpdir()` (libuv rules), `process.exit`
  semantics with exit hooks, and stdout routing (`add --json` redirects all
  human output to stderr so stdout carries exactly one JSON value).
- `color.rs` — `picocolors` (including its nested-style handling and its
  "always color on Windows" detection) and `util.styleText` as used by clack.
- `ui.rs` — the `@clack/prompts` subset (intro/outro/log/note/spinner/select/
  confirm/multiselect) including `wrap-ansi` wrapping and clack's
  exit-while-spinning "Canceled" handler.
- `collate.rs` — an approximation of `localeCompare` (ICU root collation),
  needed because the TS CLI sorts files by `localeCompare` before hashing them
  into `skills-lock.json`'s `computedHash`.
- `http.rs`, `proc.rs`, `urlutil.rs` — `fetch`, `execFile`/`spawn` with
  timeouts, and `encodeURIComponent`/`URLSearchParams`/WHATWG URL accessors.

`skills update` re-runs changed installs through this binary's own `add`
command (the TS CLI runs `node <repo>/bin/cli.mjs add ...`), never through a
shell.

## Parity harness

[`parity/parity.ps1`](parity/parity.ps1) (PowerShell 7, cross-platform) runs the reference
TypeScript CLI and the Rust binary in fresh, identical sandboxes (isolated
`HOME`/`USERPROFILE`/`TEMP`, telemetry off, agent-detection variables removed).
It compares exit codes, stdout, stderr, and the resulting file trees, including
symlinks/junctions and lock-file contents. Two local HTTP servers serve
well-known indexes and archive downloads so those flows are covered offline.

The harness needs Node.js 22.18+ and a built checkout of the reference CLI,
passed with `-Reference` or the `SKILLS_REFERENCE` environment variable:

```bash
git clone https://github.com/vercel-labs/skills ../skills
pnpm --dir ../skills install && pnpm --dir ../skills build   # update cases need dist/
cargo build --release --manifest-path rust/Cargo.toml
pwsh rust/parity/parity.ps1 -Reference ../skills                 # offline cases
pwsh rust/parity/parity.ps1 -Reference ../skills -Network        # + GitHub / skills.sh cases
pwsh rust/parity/parity.ps1 -Reference ../skills -Filter add -ShowOutput
```

Current status: 89/89 cases pass (78 offline, 11 network) on Windows against
vercel-labs/skills 1.7.0. Network cases
use the anonymous GitHub API (60 requests/hour); rerun a failing network case
before suspecting a regression.
Timing-dependent spinner frames and YAML-library error wording are normalized
before comparison. The harness is shared with the [C# port](../dotnet):
`-Bin` points it at any port binary, and `dotnet/parity/parity.ps1` does that
for the C# build. Intentional and known differences are listed in
[PARITY.md](PARITY.md).

## Cross-platform

Developed and parity-tested on Windows (x86_64-pc-windows-msvc). Linux and
macOS type-check cleanly:

```bash
cargo check --target x86_64-unknown-linux-gnu --no-default-features --all-targets
cargo check --target aarch64-apple-darwin --no-default-features
```

(`--no-default-features` only drops the TLS backend so no C cross-compiler is
needed for the check; release builds for those targets should be built natively
or with a cross toolchain.)

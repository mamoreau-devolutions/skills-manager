# skills-manager

[![CI](https://github.com/mamoreau-devolutions/skills-manager/actions/workflows/ci.yml/badge.svg)](https://github.com/mamoreau-devolutions/skills-manager/actions/workflows/ci.yml)

Native, single-executable ports of [`skills`](https://github.com/vercel-labs/skills),
the CLI for the open agent skills ecosystem. `skills` installs and manages agent
skills (folders with a `SKILL.md`) for Claude Code, Codex, Cursor, GitHub Copilot
and about 75 other coding agents.

The original CLI is a Node.js package run with `npx skills`. This repository
has two ports that build to a standalone `skills` executable with no Node.js
runtime:

| Port | Folder | Build output | Size | `skills list` startup (win-x64) |
| --- | --- | --- | --- | --- |
| Rust | [`rust/`](rust) | `rust/target/release/skills` | ~6 MB | ~35 ms |
| C# (.NET 10) | [`dotnet/`](dotnet) | `dotnet/publish/skills` | ~9 MB (NativeAOT), ~17 MB (ReadyToRun) | ~60 ms (NativeAOT), ~140 ms (ReadyToRun) |

Both follow the reference CLI (vercel-labs/skills 1.7.0) command by command
and stay compatible with it and with skills.sh: same lock files, same install
locations, same sources, same commands and flags. A shared parity harness runs
each port side by side with the original and compares output and resulting
files. They also install large skills much faster, because each skill is
copied once per run instead of once per agent.

On top of that, the ports add features of their own, described in
[`docs/EXTENSIONS.md`](docs/EXTENSIONS.md):

- `skills preview` shows a skill's files and SKILL.md without installing it.
- `skills validate` checks skills against the Agent Skills specification
  before you publish them.
- `skills add --pin <ref>` pins a skill to a tag, branch or commit (`--pin
  latest` for the newest release); `skills update` skips pinned skills.
- `skills update --dry-run`, `--force` and `--unpin`.
- `skills list` and `skills update` recognize skills installed by the GitHub
  CLI's `gh skill install`.

## Quick start

Rust (1.80+):

```bash
cargo build --release --manifest-path rust/Cargo.toml
rust/target/release/skills --help
```

C# (.NET 10 SDK):

```bash
dotnet publish dotnet/src/Skills.csproj -c Release -r win-x64 -o dotnet/publish
dotnet/publish/skills --help
```

For the C# port, replace `win-x64` with your platform (`linux-x64`,
`osx-arm64`, ...). Add `-p:PublishAot=true` for a NativeAOT build; that needs
the platform C/C++ toolchain.

Copy the executable anywhere on your `PATH`, then use it like the npm CLI:

```bash
skills add vercel-labs/agent-skills
skills add https://github.com/owner/repo/tree/main/skills/my-skill -g -a claude-code -y
skills list
skills update
skills remove my-skill
skills find typescript
skills init my-skill
skills preview vercel-labs/agent-skills@web-design-guidelines
skills validate
```

## Runtime requirements

The executables need `git` on `PATH` to clone skill repositories. They also
use the GitHub CLI (`gh`) for authentication fallbacks when present, the Notion
CLI (`ntn`) for Notion sources, and `claude`, `codex` or `sarvam-code` for
`skills use --agent`.

Like the original, the ports send anonymous usage telemetry to skills.sh. Set
`DISABLE_TELEMETRY=1` or `DO_NOT_TRACK=1` to turn it off.

## Differences from the original

Apart from saying `skills …` instead of `npx skills …` in help and hints and
the [extensions](docs/EXTENSIONS.md), the ports aim to behave identically.
Deliberate and known differences are listed in [`rust/PARITY.md`](rust/PARITY.md)
and [`dotnet/PARITY.md`](dotnet/PARITY.md).

## Testing

- Unit tests: `cargo test` in `rust/` (92 tests) and `dotnet test` in `dotnet/`
  (97 tests).
- Parity: [`rust/parity/parity.ps1`](rust/parity/parity.ps1) (PowerShell 7)
  compares a port with a built checkout of the reference CLI.
  [`dotnet/parity/parity.ps1`](dotnet/parity/parity.ps1) runs the same cases
  against the C# build. Cases for the extensions, and the help screens that
  list them, are skipped in this mode.
- Lockstep: `parity.ps1 -Lockstep` runs every case, extensions included, with
  the Rust port on one side and the C# port on the other. It needs both builds
  but not the reference CLI.
- CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs all of the
  above on Windows, Linux and macOS for every pull request: formatting, clippy,
  unit tests, a NativeAOT publish, the lockstep comparison, and both ports
  against the reference CLI at v1.7.0. A nightly run adds the network cases
  (pass `-PassGitHubToken` to use `GITHUB_TOKEN`/`GH_TOKEN` there).

```bash
git clone https://github.com/vercel-labs/skills ../skills
pnpm --dir ../skills install && pnpm --dir ../skills build
pwsh rust/parity/parity.ps1 -Reference ../skills
pwsh dotnet/parity/parity.ps1 -Reference ../skills -Network
pwsh rust/parity/parity.ps1 -Lockstep -Network
```

See [`rust/README.md`](rust/README.md) and [`dotnet/README.md`](dotnet/README.md)
for build options, module layout and more details on each port.

## License

MIT. Both ports are derived from [vercel-labs/skills](https://github.com/vercel-labs/skills),
© Vercel, Inc., and keep its license notice; see [`LICENSE`](LICENSE).

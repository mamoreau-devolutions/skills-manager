# Extensions

Features the Rust and C# ports add on top of the reference CLI
([vercel-labs/skills](https://github.com/vercel-labs/skills) 1.7.0). Both ports
implement every extension the same way, with the same output, and the
lockstep harness (`rust/parity/parity.ps1 -Lockstep`) checks that they agree.

## Compatibility contract

The ports stay interchangeable with `npx skills` and skills.sh:

- **Lock files.** `skills-lock.json` and the global lock
  (`~/.agents/.skill-lock.json`, version 3) keep their format. Extensions may
  add fields to an entry but never rename or remove existing ones. The
  reference CLI ignores the added fields.
- **Install layout.** Skills are installed to the same agent directories, with
  the same symlink or copy behavior, so either tool can list, update and remove
  what the other installed.
- **Installed files.** Installed SKILL.md files are written exactly as
  published. Extensions never write tracking data into them.
- **Sources and services.** GitHub, GitLab, git URLs, local paths, well-known
  URLs, direct downloads, Notion, the skills.sh search API and snapshot fast
  path behave as in the reference CLI.
- **Commands and flags.** Existing commands and flags keep their meaning.
  Extensions only add commands and flags.
- **Output.** Extension output appears only when extension data exists: a
  pinned skill, a `gh skill` install, or a new flag. Everything else prints
  exactly what the reference CLI prints. The help screens are the exception,
  because they list the new commands and flags.

## `skills preview`

```
skills preview <source>[@<skill>] [options]     (alias: show)
```

Shows one skill's files and its SKILL.md without installing anything. Sources
and skill selection work exactly like `skills use`: GitHub shorthand with an
optional `#ref`, git and GitLab URLs, local paths, well-known URLs and direct
downloads.

| Option | Meaning |
| --- | --- |
| `-s, --skill <skill>` | Select the skill (same as `<source>@<skill>`). |
| `--file <path>` | Print one file of the skill, raw, instead of the overview. |
| `--full-depth` | Search nested directories, like `skills add --full-depth`. |
| `--json` | Print the overview as JSON. |
| `--no-pager` | Never page the output. |

The overview lists the skill's name and description, a file tree with sizes,
and the SKILL.md content. Files that can run code (by extension: `.sh`,
`.bash`, `.zsh`, `.fish`, `.ps1`, `.psm1`, `.bat`, `.cmd`, `.py`, `.js`,
`.mjs`, `.cjs`, `.ts`, `.rb`, `.pl`, `.php`, `.lua`, `.exe`, `.dll`, `.so`,
`.dylib`) are marked `[script]`, and a warning line counts them.

On a terminal, SKILL.md headings are highlighted, long output goes through a
pager (`$PAGER`, else `less -R` when it is on `PATH`), and, when the skill has
more files, a picker lets you open them one at a time. When the output is piped,
the overview is plain text with no pager and no picker.

`--json` prints `name`, `description`, `source`, `files` (each with `path`,
`size`, `script`) and `skillMd`.

## `skills validate`

```
skills validate [path...] [options]
```

Checks skills against the [Agent Skills specification](https://agentskills.io/specification)
before you publish them. `path` can be a repository, a skill directory or a
SKILL.md file. The default is the current directory. It runs offline.

| Option | Meaning |
| --- | --- |
| `--fix` | Remove install tracking metadata (`metadata.github-*`, `metadata.local-path`) from SKILL.md files, then validate. |
| `--strict` | Exit with status 1 on warnings too, not only on errors. |
| `--json` | Print the results as JSON. |

Every SKILL.md under the path is checked, except those in hidden directories,
`node_modules` and agent install directories such as `.claude/skills`.

| Code | Severity | Check |
| --- | --- | --- |
| `frontmatter-missing` | error | SKILL.md starts with a `---` YAML frontmatter block. |
| `frontmatter-invalid` | error | The frontmatter is valid YAML and a mapping. |
| `name-missing`, `name-type` | error | `name` is present and a string. |
| `name-format` | error | `name` is 1–64 characters of lowercase letters, digits and hyphens, with no leading, trailing or doubled hyphen. |
| `name-mismatch` | error | `name` matches the skill's directory name. A warning instead for a SKILL.md at the root of the checked path. |
| `duplicate-name` | error | No two skills share a name, since they would install to the same directory. |
| `description-missing`, `description-type`, `description-empty` | error | `description` is present, a string and not blank. |
| `description-length` | error | `description` is at most 1024 characters. |
| `allowed-tools-type` | error | `allowed-tools`, if present, is a space-separated string, not a list. |
| `compatibility-length` | warning | `compatibility`, if present, is a string of at most 500 characters. |
| `metadata-type` | warning | `metadata`, if present, maps strings to strings. |
| `install-metadata` | warning | No install tracking keys under `metadata` (left behind when an installed skill is committed). `--fix` removes them. |
| `body-empty` | warning | SKILL.md has instructions after the frontmatter. |
| `broken-link` | warning | Relative Markdown links point to files that exist. |
| `link-outside-skill` | warning | Relative Markdown links stay inside the skill directory, which is all that gets installed. |
| `unknown-field` | info | Frontmatter fields outside the specification. |
| `body-long` | info | SKILL.md is at most 500 lines. |
| `installed-skills-not-ignored` | warning | Agent install directories in the repository (`.agents/skills`, `.claude/skills`, …) are gitignored, so other authors' skills are not published by accident. |

Validation exits with status 1 when it finds errors (or warnings with
`--strict`) or no skills at all.

## Pinning

```
skills add <source> --pin <ref>
skills add <source> --pin latest
```

`--pin` installs from a tag, branch or commit SHA and records the skill as
pinned. `--pin latest` resolves the repository's newest GitHub release first.
It works for GitHub, GitLab and git sources. `<source>#<ref>` still installs
from a ref without pinning it.

A pinned lock entry keeps the ref in `ref` and adds `"pinned": true`. The
reference CLI ignores the extra field.

`skills update` skips pinned skills and lists them. `--force` reinstalls them
at their pinned ref. `--unpin` includes them and moves them back to their
source's default branch.

## `skills update` options

| Option | Meaning |
| --- | --- |
| `--dry-run` | Check for updates and list them without installing, removing or prompting. |
| `--force` | Reinstall every checkable skill, even when it is up to date. This restores locally modified files. |
| `--unpin` | Include pinned skills and reinstall them from their default branch, clearing the pin. |

## Skills installed by `gh skill`

The GitHub CLI's `gh skill install` copies skills into the same agent
directories and records their origin in SKILL.md frontmatter
(`metadata.github-repo`, `github-ref`, `github-tree-sha`, `github-path`,
`github-pinned`, or `local-path`) instead of in a lock file.

- `skills list` shows the origin of such skills, marked `(gh skill)`, instead
  of `local`. `--json` adds `"managedBy": "gh"`.
- `skills update` checks them the way `gh skill update` does, against the
  latest release or else the default branch, and lists the ones with updates.
  It leaves them to gh (`gh skill update`), because reinstalling them would
  drop gh's tracking metadata.

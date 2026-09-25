#Requires -Version 7.4
<#
.SYNOPSIS
    Side-by-side parity checks: the reference TypeScript CLI (vercel-labs/skills,
    run as node <reference>/src/cli.ts) vs a native port (the Rust port by
    default; pass -Bin for another build such as the C# port).

.DESCRIPTION
    Each case runs both implementations in fresh, identical sandboxes (isolated
    HOME/USERPROFILE/TEMP, telemetry disabled, no agent-detection variables) and
    compares exit code, stdout, stderr and the resulting file trees, including
    symlinks/junctions and lock-file contents. Two local HTTP servers publish
    well-known skill indexes and archive downloads so those flows run offline.

    Requires node >= 22.18 and a checkout of the reference CLI
    (https://github.com/vercel-labs/skills) with its dependencies installed and
    built (pnpm install && pnpm build; update cases need dist/). Pass it with
    -Reference or set SKILLS_REFERENCE. Also requires a port binary: the Rust
    release build by default, or any binary passed with -Bin
    (dotnet/parity/parity.ps1 runs these cases against the C# port).

.EXAMPLE
    pwsh rust/parity/parity.ps1 -Reference ../skills           # offline cases
.EXAMPLE
    pwsh rust/parity/parity.ps1 -Reference ../skills -Network  # also GitHub / skills.sh cases
.EXAMPLE
    pwsh rust/parity/parity.ps1 -Filter add -ShowOutput        # uses $env:SKILLS_REFERENCE
#>
[CmdletBinding()]
param(
    # Include cases that talk to GitHub and skills.sh.
    [switch]$Network,
    # Checkout of the reference TypeScript CLI (default: $env:SKILLS_REFERENCE).
    [string]$Reference = $env:SKILLS_REFERENCE,
    # Only run cases whose name contains this substring.
    [string]$Filter = '',
    # Path to the port binary (default: the Rust release build).
    [string]$Bin,
    # Print both implementations' full stdout/stderr.
    [switch]$ShowOutput,
    # Keep the sandbox directories for inspection.
    [switch]$Keep,
    [int]$TimeoutSec = 180
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)

$RustDir = Split-Path -Parent $PSScriptRoot
if (-not $Reference) {
    throw 'Pass -Reference <checkout of https://github.com/vercel-labs/skills> or set SKILLS_REFERENCE (run pnpm install && pnpm build there first)'
}
if (-not (Test-Path (Join-Path $Reference 'src' 'cli.ts'))) {
    throw "No src/cli.ts under -Reference $Reference; it must be a checkout of https://github.com/vercel-labs/skills"
}
$Reference = (Resolve-Path $Reference).Path
if (-not (Test-Path (Join-Path $Reference 'dist'))) {
    Write-Warning "No dist/ under $Reference (run pnpm build there); update cases will fail"
}
if (-not $Bin) {
    $Bin = Join-Path $RustDir 'target' 'release' ($IsWindows ? 'skills.exe' : 'skills')
}
if (-not (Test-Path $Bin)) { throw "Port binary not found at $Bin (build the Rust port with cargo build --release, or pass -Bin)" }
$Node = (Get-Command node -CommandType Application | Select-Object -First 1).Source

$SkillTemplate = "---`nname: {0}`ndescription: {1}`n---`n`n# {0}`n`nBody for {0}.`n"
$Servers = @{ wk = ''; dl = '' }

# ─── File helpers ───

function Join([string]$Base, [string]$Rel) {
    if (-not $Rel) { return $Base }
    [IO.Path]::GetFullPath([IO.Path]::Combine($Base, $Rel))
}

function Write-Text([string]$Path, [string]$Content) {
    $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path))
    [IO.File]::WriteAllText($Path, $Content)   # UTF-8 without BOM, LF as written
}

function Write-Bytes([string]$Path, [byte[]]$Bytes) {
    $null = [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path))
    [IO.File]::WriteAllBytes($Path, $Bytes)
}

function Get-SkillMd([string]$Name, [string]$Desc) {
    if (-not $Desc) { $Desc = "Does $Name things" }
    $SkillTemplate -f $Name, $Desc
}

function New-Skill([string]$Root, [string]$Rel, [string]$Name, [string]$Desc, [hashtable]$Extra = @{}) {
    $dir = Join $Root $Rel
    Write-Text (Join $dir 'SKILL.md') (Get-SkillMd $Name $Desc)
    foreach ($f in $Extra.Keys) { Write-Text (Join $dir $f) $Extra[$f] }
}

function ConvertTo-JsonText($Value) { ConvertTo-Json -InputObject $Value -Depth 20 }

function Get-Sha256Hex([byte[]]$Bytes) { [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes)).ToLowerInvariant() }

function New-ZipBytes([System.Collections.Specialized.OrderedDictionary]$Entries) {
    $ms = [IO.MemoryStream]::new()
    $zip = [IO.Compression.ZipArchive]::new($ms, [IO.Compression.ZipArchiveMode]::Create, $true)
    foreach ($name in $Entries.Keys) {
        $stream = $zip.CreateEntry($name).Open()
        $data = [Text.Encoding]::UTF8.GetBytes($Entries[$name])
        $stream.Write($data, 0, $data.Length)
        $stream.Dispose()
    }
    $zip.Dispose()
    , $ms.ToArray()
}

function New-TarGzBytes([System.Collections.Specialized.OrderedDictionary]$Entries) {
    $ms = [IO.MemoryStream]::new()
    $gz = [IO.Compression.GZipStream]::new($ms, [IO.Compression.CompressionLevel]::Optimal, $true)
    $tar = [System.Formats.Tar.TarWriter]::new($gz, [System.Formats.Tar.TarEntryFormat]::Ustar, $true)
    foreach ($name in $Entries.Keys) {
        $entry = [System.Formats.Tar.UstarTarEntry]::new([System.Formats.Tar.TarEntryType]::RegularFile, $name)
        $entry.DataStream = [IO.MemoryStream]::new([Text.Encoding]::UTF8.GetBytes($Entries[$name]))
        $tar.WriteEntry($entry)
    }
    $tar.Dispose()
    $gz.Dispose()
    , $ms.ToArray()
}

# ─── Fixtures (each receives the sandbox; may return a source path for {src}) ───

$FxMultiRepo = {
    param($sb)
    $src = Join $sb.root 'src-repo'
    New-Skill $src 'skills/alpha' 'alpha' -Extra @{ 'scripts/run.sh' = "echo hi`n"; 'metadata.json' = '{}' }
    New-Skill $src 'skills/beta' 'beta' "Beta skill`nwith a multi-line description"
    New-Skill $src 'skills/cat/nested/gamma' 'gamma'
    New-Skill $src 'examples/ignored' 'ignored'
    New-Skill $src 'plugins/docs/skills/delta' 'delta'
    Write-Text (Join $src '.claude-plugin/marketplace.json') (ConvertTo-JsonText ([ordered]@{
                plugins = @([ordered]@{ name = 'doc-tools'; source = './plugins/docs'; skills = @('./skills/delta') })
            }))
    Write-Text (Join $src 'skills/alpha/.git/HEAD') "ref: x`n"
    $src
}

$FxSingle = {
    param($sb)
    $src = Join $sb.root 'single'
    New-Skill $src '' 'solo' -Extra @{ 'references/guide.md' = "guide`n"; 'README.md' = "readme`n" }
    $src
}

$FxInternal = {
    param($sb)
    $src = Join $sb.root 'internal-repo'
    Write-Text (Join $src 'skills/hidden/SKILL.md') "---`nname: hidden`ndescription: Internal`nmetadata:`n  internal: true`n---`nx`n"
    New-Skill $src 'skills/visible' 'visible'
    Write-Text (Join $src 'skills/broken/SKILL.md') "---`nname: [unclosed`n---`n"
    Write-Text (Join $src 'skills/noname/SKILL.md') "---`ndescription: missing name`n---`n"
    $src
}

$FxProjectDirs = {
    param($sb)
    $null = [IO.Directory]::CreateDirectory((Join $sb.project '.claude'))
    $null = [IO.Directory]::CreateDirectory((Join $sb.project '.cursor'))
}

$FxNodeModules = {
    param($sb)
    $nm = Join $sb.project 'node_modules'
    New-Skill $nm 'pkg-a' 'pkg-a-skill'
    New-Skill $nm '@scope/pkg-b/skills/b-one' 'b-one'
    New-Skill $nm 'pkg-c/.agents/skills/c-one' 'c-one'
    $null = [IO.Directory]::CreateDirectory((Join $nm 'no-skills'))
}

# Pre-installed project skills + lock file, used by list/remove.
$FxInstalled = {
    param($sb)
    $p = $sb.project
    New-Skill $p '.agents/skills/one' 'one'
    New-Skill $p '.agents/skills/two' 'two'
    New-Skill $p '.claude/skills/claude-only' 'claude-only'
    Write-Text (Join $p 'skills-lock.json') ((ConvertTo-JsonText ([ordered]@{
                    version = 1
                    skills  = [ordered]@{
                        'one'       = [ordered]@{ source = 'owner/repo'; sourceType = 'github'; computedHash = 'abc'; pluginName = 'my-plugin' }
                        'ce:review' = [ordered]@{ source = 'owner/repo'; sourceType = 'github'; computedHash = 'def' }
                    }
                })) + "`n")
    $null = [IO.Directory]::CreateDirectory((Join $sb.home '.claude'))
}

function Write-GlobalLock($sb, [System.Collections.Specialized.OrderedDictionary]$Skills) {
    Write-Text (Join $sb.home '.agents/.skill-lock.json') (ConvertTo-JsonText ([ordered]@{ version = 3; skills = $Skills; dismissed = [ordered]@{} }))
}

$FxGlobalLock = {
    param($sb)
    New-Skill $sb.home '.agents/skills/g-one' 'g-one'
    Write-GlobalLock $sb ([ordered]@{
            'g-one' = [ordered]@{
                source = 'local-thing'; sourceType = 'local'; sourceUrl = '/somewhere'; skillFolderHash = ''
                installedAt = '2025-01-01T00:00:00.000Z'; updatedAt = '2025-01-01T00:00:00.000Z'
            }
        })
}

# A global find-skills install whose recorded tree hash is outdated.
$FxStaleGlobalGithub = {
    param($sb)
    New-Skill $sb.home '.agents/skills/find-skills' 'find-skills'
    Write-GlobalLock $sb ([ordered]@{
            'find-skills' = [ordered]@{
                source = 'vercel-labs/skills'; sourceType = 'github'; sourceUrl = 'https://github.com/vercel-labs/skills.git'
                skillPath = 'skills/find-skills/SKILL.md'; skillFolderHash = ('0' * 40)
                installedAt = '2025-01-01T00:00:00.000Z'; updatedAt = '2025-01-01T00:00:00.000Z'
            }
        })
}

$FxStaleProjectGithub = {
    param($sb)
    New-Skill $sb.project '.agents/skills/find-skills' 'find-skills'
    Write-Text (Join $sb.project 'skills-lock.json') ((ConvertTo-JsonText ([ordered]@{
                    version = 1
                    skills  = [ordered]@{
                        'find-skills' = [ordered]@{ source = 'vercel-labs/skills'; sourceType = 'github'; skillPath = 'skills/find-skills/SKILL.md'; computedHash = 'stale' }
                    }
                })) + "`n")
}

# Global well-known install with an outdated digest.
$FxWkGlobalLock = {
    param($sb)
    New-Skill $sb.home '.agents/skills/wk-md' 'wk-md'
    Write-GlobalLock $sb ([ordered]@{
            'wk-md' = [ordered]@{
                source = 'localhost'; sourceType = 'well-known'; sourceUrl = "$($Servers.wk)/.well-known/agent-skills/wk-md/SKILL.md"
                skillFolderHash = ''; sourceBaseUrl = $Servers.wk; wellKnownDigest = 'sha256:' + ('0' * 64)
                installedAt = '2025-01-01T00:00:00.000Z'; updatedAt = '2025-01-01T00:00:00.000Z'
            }
        })
}

$FxNumericNames = {
    param($sb)
    $nm = Join $sb.project 'node_modules'
    # Quoted so YAML keeps them as strings (unquoted `name: 10` is a number and is rejected).
    Write-Text (Join $nm 'pkg-ten/SKILL.md') "---`nname: `"10`"`ndescription: Ten`n---`n"
    Write-Text (Join $nm 'pkg-nine/SKILL.md') "---`nname: `"9`"`ndescription: Nine`n---`n"
    New-Skill $nm 'pkg-alpha' 'alpha'
}

# Raw ESC/BEL characters inside frontmatter values (the yaml package accepts them).
$FxControlChars = {
    param($sb)
    $src = Join $sb.root 'ctrl'
    Write-Text (Join $src 'SKILL.md') "---`nname: colorful`ndescription: Colored `e[31mred`e[0m text`a`n---`nBody`n"
    $src
}

$FxEmptyDir = { param($sb) $d = Join $sb.root 'empty'; $null = [IO.Directory]::CreateDirectory($d); $d }
$FxInitExists = { param($sb) New-Skill $sb.project 'x' 'x' }
$FxProjectDirsAndRepo = { param($sb) $null = & $FxProjectDirs $sb; & $FxMultiRepo $sb }

# ─── Local HTTP fixtures (well-known provider + direct downloads) ───

function New-HttpFixtures([string]$Root) {
    $wk = Join $Root 'wk'
    $dl = Join $Root 'dl'
    $md = Get-SkillMd 'wk-md' 'Single-file well-known skill'
    Write-Text (Join $wk '.well-known/agent-skills/wk-md/SKILL.md') $md
    $zip = New-ZipBytes ([ordered]@{ 'SKILL.md' = (Get-SkillMd 'wk-zip' 'Archived well-known skill'); 'references/extra.md' = "extra`n" })
    Write-Bytes (Join $wk '.well-known/agent-skills/wk-zip.zip') $zip
    Write-Text (Join $wk '.well-known/agent-skills/index.json') (ConvertTo-JsonText ([ordered]@{
                '$schema' = 'https://schemas.agentskills.io/discovery/0.2.0/schema.json'
                skills    = @(
                    [ordered]@{ name = 'wk-md'; type = 'skill-md'; description = 'Single-file well-known skill'; url = 'wk-md/SKILL.md'; digest = 'sha256:' + (Get-Sha256Hex ([Text.Encoding]::UTF8.GetBytes($md))) }
                    [ordered]@{ name = 'wk-zip'; type = 'archive'; description = 'Archived well-known skill'; url = 'wk-zip.zip'; digest = 'sha256:' + (Get-Sha256Hex $zip) }
                )
            }))
    # Legacy v0.1 index under a path prefix
    Write-Text (Join $wk 'legacy/.well-known/skills/index.json') (ConvertTo-JsonText ([ordered]@{
                skills = @([ordered]@{ name = 'legacy-one'; description = 'Legacy skill'; files = @('SKILL.md', 'notes.txt') })
            }))
    Write-Text (Join $wk 'legacy/.well-known/skills/legacy-one/SKILL.md') (Get-SkillMd 'legacy-one' 'Legacy skill')
    Write-Text (Join $wk 'legacy/.well-known/skills/legacy-one/notes.txt') "notes`n"

    # An index saved with a UTF-8 BOM (common for files written on Windows)
    $bomMd = Get-SkillMd 'bom-skill' 'Index has a BOM'
    Write-Text (Join $wk 'bom/.well-known/agent-skills/bom-skill/SKILL.md') $bomMd
    $bomIndex = ConvertTo-JsonText ([ordered]@{
            '$schema' = 'https://schemas.agentskills.io/discovery/0.2.0/schema.json'
            skills    = @([ordered]@{ name = 'bom-skill'; type = 'skill-md'; description = 'Index has a BOM'; url = 'bom-skill/SKILL.md'; digest = 'sha256:' + (Get-Sha256Hex ([Text.Encoding]::UTF8.GetBytes($bomMd))) })
        })
    Write-Bytes (Join $wk 'bom/.well-known/agent-skills/index.json') ([byte[]](0xEF, 0xBB, 0xBF) + [Text.Encoding]::UTF8.GetBytes($bomIndex))

    # Direct downloads (a separate host with no well-known index)
    Write-Bytes (Join $dl 'bundle.tar.gz') (New-TarGzBytes ([ordered]@{
                'pkg/skills/tarred/SKILL.md' = (Get-SkillMd 'tarred' 'From a tarball')
                'pkg/skills/tarred/run.sh'   = "echo`n"
            }))
    Write-Bytes (Join $dl 'bundle.zip') (New-ZipBytes ([ordered]@{ 'zipped/SKILL.md' = (Get-SkillMd 'zipped' 'From a zip') }))
    Write-Text (Join $dl 'SKILL.md') (Get-SkillMd 'raw-md' 'A raw SKILL.md')
    Write-Bytes (Join $dl 'evil.zip') (New-ZipBytes ([ordered]@{ '../escape/SKILL.md' = 'x' }))
    Write-Text (Join $dl 'not-a-skill.txt') "hello world, not an archive`n"
    $wk, $dl
}

function Start-StaticServer([string]$Directory) {
    $probe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $probe.Start(); $port = $probe.LocalEndpoint.Port; $probe.Stop()
    $listener = [Net.HttpListener]::new()
    $listener.Prefixes.Add("http://localhost:$port/")
    $listener.Start()
    $job = Start-ThreadJob -ArgumentList $listener, $Directory -ScriptBlock {
        param($listener, $root)
        while ($listener.IsListening) {
            try { $ctx = $listener.GetContext() } catch { break }
            try {
                $rel = [Uri]::UnescapeDataString($ctx.Request.Url.AbsolutePath).TrimStart('/')
                $path = [IO.Path]::GetFullPath([IO.Path]::Combine($root, $rel))
                $resp = $ctx.Response
                if ($path.StartsWith($root) -and [IO.File]::Exists($path)) {
                    $bytes = [IO.File]::ReadAllBytes($path)
                    $resp.ContentType = switch -Regex ($path) {
                        '\.json$' { 'application/json' }
                        '\.zip$' { 'application/zip' }
                        '\.gz$' { 'application/gzip' }
                        '\.md$' { 'text/markdown; charset=utf-8' }
                        default { 'application/octet-stream' }
                    }
                    $resp.ContentLength64 = $bytes.Length
                    $resp.OutputStream.Write($bytes, 0, $bytes.Length)
                } else {
                    $resp.StatusCode = 404
                }
                $resp.Close()
            } catch {
                try { $ctx.Response.Abort() } catch { }
            }
        }
    }
    [pscustomobject]@{ Url = "http://localhost:$port"; Listener = $listener; Job = $job }
}

# ─── Cases ───
# Args may contain {src} (the fixture's return value), {wk} and {dl} (server URLs).

function Case([string]$Name, [string[]]$Arguments, [scriptblock]$Setup, [object[]]$Pre = @(), [switch]$Net, [switch]$Unordered, [switch]$IgnoreStderr, [switch]$StdoutShapeOnly) {
    [pscustomobject]@{
        Name = $Name; Args = @($Arguments | Where-Object { $null -ne $_ }); Setup = $Setup; Pre = $Pre; Network = [bool]$Net
        Unordered = [bool]$Unordered; IgnoreStderr = [bool]$IgnoreStderr; StdoutShapeOnly = [bool]$StdoutShapeOnly
    }
}

$Cases = @(
    Case 'version' @('--version')
    Case 'help' @('--help')
    Case 'remove-help' @('rm', '--help')
    Case 'subcommand-help' @('update', '-h')
    Case 'banner' @()
    Case 'unknown-command' @('frobnicate')
    Case 'init-named' @('init', 'my-skill')
    Case 'init-cwd' @('init')
    Case 'init-exists' @('init', 'x') $FxInitExists
    Case 'add-missing-source' @('add')
    Case 'add-bad-metadata' @('add', 'x', '--metadata', '{nope')
    Case 'add-bad-metadata-json' @('add', 'x', '--metadata', '{nope', '--json')
    Case 'add-local-list' @('add', '{src}', '--list') $FxMultiRepo
    Case 'add-local-all-project' @('add', '{src}', '-y', '-a', 'claude-code', 'cursor', 'windsurf') $FxMultiRepo
    Case 'add-local-skill-filter' @('add', '{src}', '-y', '-s', 'beta', 'ALPHA', 'nope', '-a', 'claude-code') $FxMultiRepo
    Case 'add-local-copy' @('add', '{src}', '-y', '--copy', '-a', 'claude-code', 'codex') $FxMultiRepo
    Case 'add-local-project-dirs' @('add', '{src}', '-y', '-a', 'claude-code', 'cursor', 'windsurf') $FxProjectDirsAndRepo
    Case 'add-local-global' @('add', '{src}', '-g', '-y', '-a', 'claude-code', 'codex') $FxSingle
    Case 'add-local-full-depth' @('add', '{src}', '--full-depth', '--list') $FxSingle
    Case 'add-local-json' @('add', '{src}', '--json', '-y', '-a', 'claude-code', '-s', 'alpha', 'missing') $FxMultiRepo
    Case 'add-local-json-needs-yes' @('add', '{src}', '--json') $FxMultiRepo
    Case 'add-local-invalid-agent' @('add', '{src}', '-y', '-a', 'not-an-agent') $FxSingle
    Case 'add-local-no-skills' @('add', '{src}', '-y') $FxEmptyDir
    Case 'add-local-missing-path' @('add', './does-not-exist', '-y')
    Case 'add-local-internal' @('add', '{src}', '--list') $FxInternal
    Case 'add-local-internal-explicit' @('add', '{src}', '-y', '-s', 'hidden', '-a', 'claude-code') $FxInternal
    Case 'add-control-chars' @('add', '{src}', '-y', '-a', 'claude-code') $FxControlChars
    Case 'use-control-chars' @('use', '{src}') $FxControlChars
    Case 'add-subpath-traversal' @('add', 'owner/repo/../../etc', '-y')
    Case 'add-eve' @('add', '{src}', '-y', '--subagent', 'root', 'research') $FxSingle
    Case 'list-empty' @('list')
    Case 'list-project' @('list') $FxInstalled
    Case 'list-json' @('ls', '--json') $FxInstalled
    Case 'list-agent-filter' @('ls', '-a', 'claude-code') $FxInstalled
    Case 'list-invalid-agent' @('ls', '-a', 'nope')
    Case 'list-global' @('ls', '-g') $FxGlobalLock
    Case 'remove-by-name' @('remove', 'one', '-y') $FxInstalled
    Case 'remove-lock-only' @('remove', 'ce:review', '-y') $FxInstalled
    Case 'remove-all' @('remove', '--all') $FxInstalled
    Case 'remove-all-with-names' @('remove', 'one', '--all') $FxInstalled
    Case 'remove-no-match' @('remove', 'zzz', '-y') $FxInstalled
    Case 'remove-nothing' @('remove', '-y')
    Case 'use-local' @('use', '{src}', '--skill', 'beta') $FxMultiRepo
    Case 'use-local-single' @('use', '{src}') $FxSingle
    Case 'use-multiple' @('use', '{src}') $FxMultiRepo
    Case 'use-no-match' @('use', '{src}', '-s', 'zzz') $FxMultiRepo
    Case 'use-bad-agent' @('use', '{src}', '--agent', 'cursor') $FxSingle
    Case 'use-errors' @('use', '--agent', '*', '--bogus')
    # Known divergences (see rust/PARITY.md): TS discovers node_modules skills in
    # promise-completion order and warns for every package without a root SKILL.md.
    Case 'sync' @('experimental_sync', '-y', '-a', 'claude-code') $FxNodeModules -Unordered -IgnoreStderr
    Case 'sync-nothing' @('experimental_sync', '-y')
    Case 'sync-numeric-names' @('experimental_sync', '-y', '-a', 'claude-code') $FxNumericNames -Unordered -IgnoreStderr
    # Symlink-mode installs create junctions on Windows; removal must delete the links.
    Case 'remove-linked' @('remove', 'pkg-a-skill', '-y') $FxNodeModules -Pre @(, @('experimental_sync', '-y', '-a', 'claude-code'))
    Case 'remove-linked-one-agent' @('remove', 'pkg-a-skill', '-y', '-a', 'claude-code') $FxNodeModules -Pre @(, @('experimental_sync', '-y', '-a', 'claude-code'))
    Case 'reinstall-over-link' @('add', '{src}', '-y', '-a', 'claude-code') $FxMultiRepo -Pre @(, @('add', '{src}', '-y', '-a', 'claude-code', '-s', 'alpha'))
    Case 'install-empty-lock' @('experimental_install')
    Case 'update-no-skills' @('update', '-y')
    Case 'update-global-skipped' @('update', '-g') $FxGlobalLock
    Case 'update-filter-miss' @('update', 'nothing-here')
    Case 'wk-list' @('add', '{wk}', '--list')
    Case 'wk-install-project' @('add', '{wk}', '-y', '-a', 'claude-code', 'cursor')
    Case 'wk-install-global-filter' @('add', '{wk}', '-g', '-y', '-s', 'WK-ZIP', '-a', 'claude-code')
    Case 'wk-install-no-match' @('add', '{wk}', '-y', '-s', 'nope')
    Case 'wk-legacy' @('add', '{wk}/legacy', '-y', '-a', 'codex')
    Case 'wk-bom-index' @('add', '{wk}/bom', '--list')
    Case 'wk-scope-missing' @('add', '{wk}/scoped/path', '-y')
    Case 'wk-json-unsupported' @('add', '{wk}', '--json', '-y')
    Case 'wk-use' @('use', '{wk}', '--skill', 'wk-zip')
    Case 'wk-use-multiple' @('use', '{wk}')
    Case 'wk-update-global' @('update', '-g', '-y') $FxWkGlobalLock
    Case 'dl-tgz' @('add', '{dl}/bundle.tar.gz', '-y', '-a', 'claude-code')
    Case 'dl-zip' @('add', '{dl}/bundle.zip', '-y', '--copy', '-a', 'claude-code', 'codex')
    Case 'dl-skill-md' @('add', '{dl}/SKILL.md', '-y', '-a', 'claude-code')
    Case 'dl-zip-traversal' @('add', '{dl}/evil.zip', '-y')
    Case 'dl-not-archive' @('add', '{dl}/not-a-skill.txt', '-y')
    Case 'dl-404' @('add', '{dl}/missing.zip', '-y')
    Case 'dl-use-tgz' @('use', '{dl}/bundle.tar.gz')
    Case 'find-errors' @('find', '--owner')
    Case 'find-noninteractive' @('find')
    # Network cases
    Case 'net-find' @('find', 'typescript') -Net -StdoutShapeOnly
    Case 'net-add-blob-list' @('add', 'vercel-labs/skills', '--list') -Net
    Case 'net-add-blob-global' @('add', 'vercel-labs/skills@find-skills', '-g', '-y', '-a', 'claude-code', 'codex') -Net
    Case 'net-add-blob-project-json' @('add', 'vercel-labs/skills', '-s', 'find-skills', '-y', '--json', '-a', 'claude-code') -Net
    Case 'net-add-clone-list' @('add', 'https://github.com/anthropics/skills', '--list') -Net
    Case 'net-add-clone-ref' @('add', 'vercel-labs/skills#main', '-s', 'find-skills', '-y', '-a', 'claude-code') -Net
    Case 'net-use-blob' @('use', 'vercel-labs/skills@find-skills') -Net
    Case 'net-update-global' @('update', '-g', '-y') $FxStaleGlobalGithub -Net
    Case 'net-update-project' @('update', '-p', '-y') $FxStaleProjectGithub -Net
    Case 'net-check-filter' @('check', 'find-skills', '-g') $FxStaleGlobalGithub -Net
    Case 'net-download-raw' @('add', 'https://raw.githubusercontent.com/vercel-labs/skills/main/skills/find-skills/SKILL.md', '-y', '-a', 'claude-code') -Net
)

# ─── Runner ───

function New-Sandbox([string]$Base, [string]$Impl, [string]$CaseName) {
    $root = Join $Base "$Impl/$CaseName"
    $sb = @{ root = $root; home = (Join $root 'home'); project = (Join $root 'project'); tmp = (Join $root 'tmp') }
    foreach ($k in 'home', 'project', 'tmp') { $null = [IO.Directory]::CreateDirectory($sb[$k]) }
    $sb
}

function Get-CleanEnv($sb) {
    $vars = [ordered]@{}
    foreach ($k in 'PATH', 'SystemRoot', 'windir', 'COMSPEC', 'PATHEXT', 'SystemDrive', 'ProgramFiles', 'ProgramData', 'LANG') {
        $v = [Environment]::GetEnvironmentVariable($k)
        if ($null -ne $v) { $vars[$k] = $v }
    }
    $vars.HOME = $sb.home
    $vars.USERPROFILE = $sb.home
    $vars.APPDATA = Join $sb.home 'AppData/Roaming'
    $vars.LOCALAPPDATA = Join $sb.home 'AppData/Local'
    $vars.TEMP = $sb.tmp
    $vars.TMP = $sb.tmp
    $vars.TMPDIR = $sb.tmp
    $vars.DISABLE_TELEMETRY = '1'
    $vars.GIT_CONFIG_NOSYSTEM = '1'
    $vars
}

function ConvertTo-Normalized([string]$Text, $sb) {
    $root = $sb.root
    # Select-Object -Unique is case-sensitive; Sort-Object -Unique would dedupe by Length here.
    $variants = @($root, $root.Replace('\', '/'), $root.Replace('\', '\\')) | Select-Object -Unique | Sort-Object -Property Length -Descending
    foreach ($v in $variants) { $Text = $Text.Replace($v, '<ROOT>') }
    $Text = $Text -replace 'skills-(download-|use-|notion-)?[A-Za-z0-9]{6}', 'skills-$1XXXXXX'
    $Text = $Text -replace '\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z', '<TS>'
    $Text = $Text.Replace("`r`n", "`n")
    # The ports are standalone executables, so their hints say `skills …`
    # where the npm CLI says `npx skills …`.
    $Text = $Text.Replace('npx skills', 'skills')
    # Spinner animation frames depend on timing; drop them (each frame ends
    # with the clear sequence ESC[1G ESC[J).
    do {
        $prev = $Text
        $Text = $Text -replace '(^|\n|\x1b\[J)[^\n]*?\x1b\[1G\x1b\[J', '$1'
    } while ($prev -ne $Text)
    # The two YAML libraries word their parse errors differently.
    $Text -replace 'YAML parse error: [^\n]*\n(?:\n(?:[^\n]*\n)*?[ \t]*\^+\n\n?)?', "YAML parse error: <msg>`n"
}

function Get-TreeSnapshot($sb) {
    $out = [ordered]@{}
    foreach ($label in 'home', 'project') {
        $base = $sb[$label]
        $stack = [Collections.Generic.Stack[string]]::new()
        $stack.Push($base)
        while ($stack.Count -gt 0) {
            $dir = $stack.Pop()
            foreach ($item in ([IO.DirectoryInfo]::new($dir).EnumerateFileSystemInfos() | Sort-Object Name)) {
                $rel = [IO.Path]::GetRelativePath($base, $item.FullName).Replace('\', '/')
                if ($rel.StartsWith('AppData')) { continue }
                $key = "$label/$rel"
                if ($item -is [IO.DirectoryInfo]) {
                    if ($item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)) {
                        $target = try { $item.ResolveLinkTarget($true).FullName } catch { "<broken:$($item.LinkTarget)>" }
                        $out[$key] = 'link -> ' + (ConvertTo-Normalized $target $sb).Replace('\', '/')
                    } else {
                        $stack.Push($item.FullName)
                    }
                    continue
                }
                $bytes = [IO.File]::ReadAllBytes($item.FullName)
                if ($item.Name.EndsWith('.json') -or $item.Name -eq 'SKILL.md') {
                    $out[$key] = ConvertTo-Normalized ([Text.Encoding]::UTF8.GetString($bytes)) $sb
                } else {
                    $out[$key] = 'sha256:' + (Get-Sha256Hex $bytes).Substring(0, 16)
                }
            }
        }
    }
    $out
}

function Invoke-Cli([string[]]$Command, [string[]]$Arguments, $sb, $Src) {
    $psi = [Diagnostics.ProcessStartInfo]::new($Command[0])
    foreach ($a in (@($Command | Select-Object -Skip 1) + $Arguments)) {
        if ($Src -is [string]) { $a = $a.Replace('{src}', $Src) }
        $psi.ArgumentList.Add($a.Replace('{wk}', $Servers.wk).Replace('{dl}', $Servers.dl))
    }
    $psi.WorkingDirectory = $sb.project
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $psi.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $psi.Environment.Clear()
    $cleanEnv = Get-CleanEnv $sb
    foreach ($k in $cleanEnv.Keys) { $psi.Environment[$k] = $cleanEnv[$k] }

    $proc = [Diagnostics.Process]::Start($psi)
    $proc.StandardInput.Close()   # like stdin=DEVNULL: prompts see EOF / no TTY
    $stdout = $proc.StandardOutput.ReadToEndAsync()
    $stderr = $proc.StandardError.ReadToEndAsync()
    $code = if ($proc.WaitForExit($TimeoutSec * 1000)) { $proc.ExitCode } else { $proc.Kill($true); 'TIMEOUT' }
    [pscustomobject]@{ Code = $code; Stdout = $stdout.Result; Stderr = $stderr.Result }
}

function Invoke-Impl([string]$Impl, [string[]]$Command, $Case, [string]$Base) {
    $sb = New-Sandbox $Base $Impl $Case.Name
    $src = if ($Case.Setup) { & $Case.Setup $sb } else { $null }
    # Preparatory commands run with the same implementation; only their effects count.
    foreach ($pre in $Case.Pre) { $null = Invoke-Cli $Command $pre $sb $src }
    $r = Invoke-Cli $Command $Case.Args $sb $src
    [pscustomobject]@{
        Code   = $r.Code
        Stdout = ConvertTo-Normalized $r.Stdout $sb
        Stderr = ConvertTo-Normalized $r.Stderr $sb
        Tree   = Get-TreeSnapshot $sb
    }
}

# Line diff (LCS) rendered as unified-style hunks with 2 lines of context.
function Get-LineDiff([string]$Label, [string]$A, [string]$B) {
    if ($A -ceq $B) { return @() }
    $la = $A -split "`n"; $lb = $B -split "`n"
    $n = $la.Count; $m = $lb.Count
    $dp = [int[, ]]::new($n + 1, $m + 1)
    for ($i = $n - 1; $i -ge 0; $i--) {
        for ($j = $m - 1; $j -ge 0; $j--) {
            $dp[$i, $j] = if ($la[$i] -ceq $lb[$j]) { $dp[($i + 1), ($j + 1)] + 1 } else { [Math]::Max($dp[($i + 1), $j], $dp[$i, ($j + 1)]) }
        }
    }
    $ops = [Collections.Generic.List[string]]::new()
    $i = 0; $j = 0
    while ($i -lt $n -or $j -lt $m) {
        if ($i -lt $n -and $j -lt $m -and $la[$i] -ceq $lb[$j]) { $ops.Add(' ' + $la[$i]); $i++; $j++ }
        elseif ($j -lt $m -and ($i -ge $n -or $dp[$i, ($j + 1)] -ge $dp[($i + 1), $j])) { $ops.Add('+' + $lb[$j]); $j++ }
        else { $ops.Add('-' + $la[$i]); $i++ }
    }
    $keep = [bool[]]::new($ops.Count)
    for ($k = 0; $k -lt $ops.Count; $k++) {
        if ($ops[$k][0] -ne ' ') { for ($c = [Math]::Max(0, $k - 2); $c -le [Math]::Min($ops.Count - 1, $k + 2); $c++) { $keep[$c] = $true } }
    }
    $lines = [Collections.Generic.List[string]]::new()
    $lines.Add("--- $Label (ts)"); $lines.Add("+++ $Label (port)")
    $gap = $true
    for ($k = 0; $k -lt $ops.Count; $k++) {
        if ($keep[$k]) { if ($gap) { $lines.Add('@@') }; $lines.Add($ops[$k]); $gap = $false } else { $gap = $true }
    }
    $lines
}

$base = Join ([IO.Path]::GetTempPath()) ('skills-parity-' + [IO.Path]::GetRandomFileName().Replace('.', ''))
$wkDir, $dlDir = New-HttpFixtures (Join $base 'http')
$serverHandles = @((Start-StaticServer $wkDir), (Start-StaticServer $dlDir))
$Servers.wk = $serverHandles[0].Url
$Servers.dl = $serverHandles[1].Url

$tsCommand = @($Node, (Join $Reference 'src/cli.ts'))
$rsCommand = @($Bin)
$passed = 0
$failed = [Collections.Generic.List[string]]::new()
try {
    foreach ($case in $Cases) {
        if (-not $case.Name.Contains($Filter) -or ($case.Network -and -not $Network)) { continue }
        $ts = Invoke-Impl 'ts' $tsCommand $case $base
        $rs = Invoke-Impl 'rs' $rsCommand $case $base
        $problems = [Collections.Generic.List[string]]::new()
        if ("$($ts.Code)" -ne "$($rs.Code)") { $problems.Add("exit code: ts=$($ts.Code) port=$($rs.Code)") }
        if ($case.StdoutShapeOnly) {
            if ([bool]$ts.Stdout.Trim() -ne [bool]$rs.Stdout.Trim()) { $problems.Add('stdout presence differs') }
        } else {
            $aOut = $ts.Stdout; $bOut = $rs.Stdout
            if ($case.Unordered) {
                $aOut = ($aOut -split "`n" | Sort-Object -CaseSensitive) -join "`n"
                $bOut = ($bOut -split "`n" | Sort-Object -CaseSensitive) -join "`n"
            }
            foreach ($l in (Get-LineDiff 'stdout' $aOut $bOut)) { $problems.Add($l) }
            if (-not $case.IgnoreStderr) { foreach ($l in (Get-LineDiff 'stderr' $ts.Stderr $rs.Stderr)) { $problems.Add($l) } }
        }
        $keys = @($ts.Tree.Keys) + @($rs.Tree.Keys) | Sort-Object -Unique -CaseSensitive
        foreach ($k in $keys) {
            $inTs = $ts.Tree.Contains($k); $inRs = $rs.Tree.Contains($k)
            if (-not $inTs) { $problems.Add("tree: only in port: $k") }
            elseif (-not $inRs) { $problems.Add("tree: only in ts: $k") }
            elseif ($ts.Tree[$k] -cne $rs.Tree[$k]) { foreach ($l in (Get-LineDiff "file $k" $ts.Tree[$k] $rs.Tree[$k])) { $problems.Add($l) } }
        }
        if ($problems.Count -eq 0) {
            $passed++
            Write-Host "[PASS] $($case.Name)" -ForegroundColor Green
        } else {
            $failed.Add($case.Name)
            Write-Host "[FAIL] $($case.Name)" -ForegroundColor Red
            $problems | Select-Object -First 80 | ForEach-Object { Write-Host "    $_" }
        }
        if ($ShowOutput) {
            foreach ($pair in @(@('ts', $ts), @('port', $rs))) {
                Write-Host "  ── $($pair[0]) (exit $($pair[1].Code)) stdout:`n$($pair[1].Stdout)`n  ── $($pair[0]) stderr:`n$($pair[1].Stderr)"
            }
        }
    }
} finally {
    foreach ($h in $serverHandles) {
        try { $h.Listener.Stop(); $h.Listener.Close() } catch { }
        Stop-Job $h.Job -ErrorAction SilentlyContinue
        Remove-Job $h.Job -Force -ErrorAction SilentlyContinue
    }
    if ($Keep) { Write-Host "sandboxes kept in $base" } else { Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue }
}

Write-Host "`n$passed passed, $($failed.Count) failed"
exit ($failed.Count -gt 0 ? 1 : 0)

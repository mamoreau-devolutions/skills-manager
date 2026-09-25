//! `skills` — the CLI for the open agent skills ecosystem.
//!
//! A self-contained Rust port of the TypeScript CLI at
//! https://github.com/vercel-labs/skills (entry point: `src/cli.ts`). Module
//! names mirror the TS files they port.

pub mod add;
pub mod agents;
pub mod archive;
pub mod blob;
pub mod collate;
pub mod color;
pub mod detect_agent;
pub mod download_source;
pub mod find;
pub mod frontmatter;
pub mod git;
pub mod github_host;
pub mod http;
pub mod install_lock;
pub mod installer;
pub mod list;
pub mod local_lock;
pub mod notion;
pub mod paths;
pub mod plugin_manifest;
pub mod proc;
pub mod remove;
pub mod sanitize;
pub mod search_multiselect;
pub mod skill_lock;
pub mod skill_relocation;
pub mod skills;
pub mod source_parser;
pub mod sync;
pub mod sys;
pub mod telemetry;
pub mod types;
pub mod ui;
pub mod update;
pub mod update_source;
pub mod urlutil;
pub mod use_cmd;
pub mod wellknown;

use color::ansi::{BOLD, DIM, RESET, TEXT};
use std::time::Duration;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const LOGO_LINES: [&str; 6] = [
    "███████╗██╗  ██╗██╗██╗     ██╗     ███████╗",
    "██╔════╝██║ ██╔╝██║██║     ██║     ██╔════╝",
    "███████╗█████╔╝ ██║██║     ██║     ███████╗",
    "╚════██║██╔═██╗ ██║██║     ██║     ╚════██║",
    "███████║██║  ██╗██║███████╗███████╗███████║",
    "╚══════╝╚═╝  ╚═╝╚═╝╚══════╝╚══════╝╚══════╝",
];

const GRAYS: [&str; 6] = [
    "\x1b[38;5;250m",
    "\x1b[38;5;248m",
    "\x1b[38;5;245m",
    "\x1b[38;5;243m",
    "\x1b[38;5;240m",
    "\x1b[38;5;238m",
];

fn show_logo() {
    outln!();
    for (i, line) in LOGO_LINES.iter().enumerate() {
        outln!("{}{}{}", GRAYS[i], line, RESET);
    }
}

fn show_banner() {
    show_logo();
    outln!();
    outln!("{}The open agent skills ecosystem{}", DIM, RESET);
    outln!();
    outln!(
        "  {d}${r} {t}skills add {d}<package>{r}        {d}Add a new skill{r}",
        d = DIM,
        r = RESET,
        t = TEXT
    );
    outln!(
        "  {d}${r} {t}skills use {d}<package>@<skill>{r} {d}Use a skill without installing{r}",
        d = DIM,
        r = RESET,
        t = TEXT
    );
    outln!(
        "  {d}${r} {t}skills remove{r}               {d}Remove installed skills{r}",
        d = DIM,
        r = RESET,
        t = TEXT
    );
    outln!(
        "  {d}${r} {t}skills list{r}                 {d}List installed skills{r}",
        d = DIM,
        r = RESET,
        t = TEXT
    );
    outln!(
        "  {d}${r} {t}skills find {d}[query]{r}         {d}Search for skills{r}",
        d = DIM,
        r = RESET,
        t = TEXT
    );
    outln!();
    outln!(
        "  {d}${r} {t}skills update{r}               {d}Update installed skills{r}",
        d = DIM,
        r = RESET,
        t = TEXT
    );
    outln!();
    outln!(
        "  {d}${r} {t}skills experimental_install{r} {d}Restore from skills-lock.json{r}",
        d = DIM,
        r = RESET,
        t = TEXT
    );
    outln!(
        "  {d}${r} {t}skills init {d}[name]{r}          {d}Create a new skill{r}",
        d = DIM,
        r = RESET,
        t = TEXT
    );
    outln!(
        "  {d}${r} {t}skills experimental_sync{r}    {d}Sync skills from node_modules{r}",
        d = DIM,
        r = RESET,
        t = TEXT
    );
    outln!();
    outln!("{}try:{} skills add vercel-labs/agent-skills", DIM, RESET);
    outln!();
    outln!(
        "Discover more skills at {}https://skills.sh/{}",
        TEXT,
        RESET
    );
    outln!();
}

fn show_help() {
    let (b, d, r, t) = (BOLD, DIM, RESET, TEXT);
    outln!(
        "
{b}Usage:{r} skills <command> [options]

{b}Manage Skills:{r}
  add <package>        Add a skill package (alias: a)
                       e.g. vercel-labs/agent-skills
                            notion
                            https://notion.so/<skill-page>
                            https://github.com/vercel-labs/agent-skills
  use <package>@<skill>
                       Generate a prompt for using one skill without installing it
  remove [skills]      Remove installed skills
  list, ls             List installed skills
  find [query]         Search for skills interactively

{b}Find Options:{r}
  --owner <owner>        Search only repositories from a GitHub owner

{b}Updates:{r}
  update [skills...]   Update skills to latest versions (alias: upgrade)

{b}Update Options:{r}
  -g, --global           Update global skills only
  -p, --project          Update project skills only
  -y, --yes              Skip scope prompt (auto-detect: project if in a project, else global)

{b}Project:{r}
  experimental_install Restore skills from skills-lock.json
  init [name]          Initialize a skill (creates <name>/SKILL.md or ./SKILL.md)
  experimental_sync    Sync skills from node_modules into agent directories

{b}Add Options:{r}
  -g, --global           Install skill globally (user-level) instead of project-level
  -a, --agent <agents>   Specify agents to install to (use '*' for all agents)
  -s, --skill <skills>   Specify skill names to install (use '*' for all skills)
  -l, --list             List available skills in the repository without installing
  -y, --yes              Skip confirmation prompts
  --copy                 Copy files instead of symlinking to agent directories
  --metadata <json>      Attach valid JSON to the install telemetry event
  --subagent <names>     Install to Eve subagents (use 'root' for the root agent)
  --all                  Shorthand for --skill '*' --agent '*' -y
  --full-depth           Search all subdirectories even when a root SKILL.md exists
  --json                 Output results as JSON (machine-readable, no ANSI codes)

{b}Use Options:{r}
  -s, --skill <skill>    Specify the skill to use
  -a, --agent <agent>    Start one supported agent interactively
  --full-depth           Search all subdirectories even when a root SKILL.md exists

{b}Remove Options:{r}
  -g, --global           Remove from global scope
  -a, --agent <agents>   Remove from specific agents (omit to clean all agent links)
  -s, --skill <skills>   Specify skills to remove (use '*' for all skills)
  -y, --yes              Skip confirmation prompts
  --all                  Remove every installed skill (-y implied). Do not combine with named skills.
  
{b}Experimental Sync Options:{r}
  -a, --agent <agents>   Specify agents to install to (use '*' for all agents)
  -y, --yes              Skip confirmation prompts

{b}List Options:{r}
  -g, --global           List global skills (default: project)
  -a, --agent <agents>   Filter by specific agents
  --json                 Output as JSON (machine-readable, no ANSI codes)

{b}Options:{r}
  --help, -h        Show this help message
  --version, -v     Show version number

{b}Examples:{r}
  {d}${r} skills add vercel-labs/agent-skills
  {d}${r} skills use vercel-labs/agent-skills@vercel-optimize | claude
  {d}${r} skills use vercel-labs/agent-skills --skill vercel-optimize --agent claude-code
  {d}${r} skills add vercel-labs/agent-skills -g
  {d}${r} skills add vercel-labs/agent-skills --agent claude-code cursor
  {d}${r} skills add vercel-labs/agent-skills --skill pr-review commit
  {d}${r} skills add vercel-labs/agent-skills --json -y {d}# JSON output{r}
  {d}${r} skills remove                        {d}# interactive remove{r}
  {d}${r} skills remove web-design             {d}# remove by name{r}
  {d}${r} skills rm --global frontend-design
  {d}${r} skills list                          {d}# list project skills{r}
  {d}${r} skills ls -g                         {d}# list global skills{r}
  {d}${r} skills ls -a claude-code             {d}# filter by agent{r}
  {d}${r} skills ls --json                      {d}# JSON output{r}
  {d}${r} skills find                          {d}# interactive search{r}
  {d}${r} skills find typescript               {d}# search by keyword{r}
  {d}${r} skills find react --owner vercel     {d}# search within an owner{r}
  {d}${r} skills update
  {d}${r} skills update my-skill             {d}# update a single skill{r}
  {d}${r} skills update -g                    {d}# update global skills only{r}
  {d}${r} skills experimental_install            {d}# restore from skills-lock.json{r}
  {d}${r} skills init my-skill
  {d}${r} skills experimental_sync              {d}# sync from node_modules{r}
  {d}${r} skills experimental_sync -y           {d}# sync without prompts{r}

Discover more skills at {t}https://skills.sh/{r}
"
    );
}

fn show_remove_help() {
    let (b, d, r, t) = (BOLD, DIM, RESET, TEXT);
    outln!(
        "
{b}Usage:{r} skills remove [skills...] [options]

{b}Description:{r}
  Remove installed skills from agents. If no skill names are provided,
  an interactive selection menu will be shown.

{b}Arguments:{r}
  skills            Optional skill names to remove (space-separated)

{b}Options:{r}
  -g, --global       Remove from global scope (~/) instead of project scope
  -a, --agent        Remove from specific agents (omit to clean all agent links)
  -s, --skill        Specify skills to remove (use '*' for all skills)
  -y, --yes          Skip confirmation prompts
  --all              Remove every installed skill (-y implied). Do not combine with named skills.

{b}Examples:{r}
  {d}${r} skills remove                           {d}# interactive selection{r}
  {d}${r} skills remove my-skill                   {d}# remove specific skill{r}
  {d}${r} skills remove skill1 skill2 -y           {d}# remove multiple skills{r}
  {d}${r} skills remove --global my-skill          {d}# remove from global scope{r}
  {d}${r} skills rm --agent claude-code my-skill   {d}# remove from specific agent{r}
  {d}${r} skills remove --all                      {d}# remove all skills{r}
  {d}${r} skills remove --skill '*' -a cursor      {d}# remove all skills from cursor{r}

Discover more skills at {t}https://skills.sh/{r}
"
    );
}

fn run_init(args: &[String]) {
    let cwd = sys::cwd();
    let has_name = !args.is_empty();
    let skill_name = if has_name && !args[0].is_empty() {
        args[0].clone()
    } else {
        paths::basename(&cwd)
    };
    let skill_dir = if has_name {
        paths::join(&[cwd.as_str(), skill_name.as_str()])
    } else {
        cwd.clone()
    };
    let skill_file = paths::join(&[skill_dir.as_str(), "SKILL.md"]);
    let display = if has_name {
        format!("{}/SKILL.md", skill_name)
    } else {
        "SKILL.md".to_string()
    };

    if std::path::Path::new(&skill_file).exists() {
        outln!(
            "{}Skill already exists at {}{}{}",
            TEXT,
            DIM,
            display,
            RESET
        );
        return;
    }
    if has_name {
        if let Err(e) = std::fs::create_dir_all(&skill_dir) {
            errln!("{}", e);
            sys::exit(1);
        }
    }
    let content = format!(
        "---\nname: {n}\ndescription: A brief description of what this skill does\n---\n\n# {n}\n\nInstructions for the agent to follow when this skill is activated.\n\n## When to use\n\nDescribe when this skill should be used.\n\n## Instructions\n\n1. First step\n2. Second step\n3. Additional steps as needed\n",
        n = skill_name
    );
    if let Err(e) = std::fs::write(&skill_file, content) {
        errln!("{}", e);
        sys::exit(1);
    }
    outln!("{}Initialized skill: {}{}{}", TEXT, DIM, skill_name, RESET);
    outln!();
    outln!("{}Created:{}", DIM, RESET);
    outln!("  {}", display);
    outln!();
    outln!("{}Next steps:{}", DIM, RESET);
    outln!(
        "  1. Edit {}{}{} to define your skill instructions",
        TEXT,
        display,
        RESET
    );
    outln!(
        "  2. Update the {t}name{r} and {t}description{r} in the frontmatter",
        t = TEXT,
        r = RESET
    );
    outln!();
    outln!("{}Publishing:{}", DIM, RESET);
    outln!(
        "  {d}GitHub:{r}  Push to a repo, then {t}skills add <owner>/<repo>{r}",
        d = DIM,
        r = RESET,
        t = TEXT
    );
    outln!(
        "  {d}URL:{r}     Host the file, then {t}skills add https://example.com/{p}{r}",
        d = DIM,
        r = RESET,
        t = TEXT,
        p = display
    );
    outln!();
    outln!(
        "Browse existing skills for inspiration at {}https://skills.sh/{}",
        TEXT,
        RESET
    );
    outln!();
}

fn run(args: Vec<String>) {
    let in_agent = detect_agent::is_running_in_agent();
    if args.is_empty() {
        if !in_agent {
            show_banner();
        }
        return;
    }
    let command = args[0].as_str();
    let rest: Vec<String> = args[1..].to_vec();

    // Subcommand --help / -h short-circuits before dispatch so side-effecting
    // handlers never run when help was requested.
    if !matches!(command, "--help" | "-h" | "--version" | "-v")
        && rest.iter().any(|a| a == "--help" || a == "-h")
    {
        if matches!(command, "remove" | "rm" | "r") {
            show_remove_help();
        } else {
            show_help();
        }
        return;
    }

    match command {
        "find" | "search" | "f" | "s" => {
            if !in_agent {
                show_logo();
            }
            outln!();
            find::run_find(&rest);
        }
        "init" => {
            if !in_agent {
                show_logo();
            }
            outln!();
            run_init(&rest);
        }
        "experimental_install" => {
            if !in_agent {
                show_logo();
            }
            install_lock::run_install_from_lock(&rest);
        }
        "i" | "install" | "a" | "add" => {
            let (source, opts, errors) = add::parse_add_options(&rest);
            if !in_agent && !opts.json {
                show_logo();
            }
            if !errors.is_empty() {
                for e in &errors {
                    errln!("Error: {}", e);
                }
                if opts.json {
                    outln!("[]");
                }
                sys::set_exit_code(1);
                return;
            }
            add::run_add(&source, opts);
        }
        "use" => {
            let (source, opts, errors) = use_cmd::parse_use_options(&rest);
            use_cmd::run_use(&source, &opts, &errors);
        }
        "remove" | "rm" | "r" => {
            let (skills, opts) = remove::parse_remove_options(&rest);
            remove::remove_command(skills, opts);
        }
        "experimental_sync" => {
            if !in_agent {
                show_logo();
            }
            let opts = sync::parse_sync_options(&rest);
            sync::run_sync(&rest, opts);
        }
        "list" | "ls" => list::run_list(&rest),
        "check" | "update" | "upgrade" => update::run_update(&rest),
        "--help" | "-h" => show_help(),
        "--version" | "-v" => outln!("{}", VERSION),
        _ => {
            outln!("Unknown command: {}", command);
            outln!("Run {}skills --help{} for usage.", BOLD, RESET);
            sys::set_exit_code(1);
        }
    }
}

fn main() {
    #[cfg(windows)]
    {
        // Node translates ANSI sequences for the Windows console; enable VT
        // processing so colors and cursor movement render the same way.
        let _ = crossterm::ansi_support::supports_ansi();
    }
    telemetry::set_version(VERSION);
    let args: Vec<String> = std::env::args().skip(1).collect();
    run(args);
    telemetry::flush_telemetry(Duration::from_secs(5));
    ui::on_process_exit(sys::exit_code());
    ui::restore_terminal();
    std::process::exit(sys::exit_code());
}

#[cfg(test)]
mod tests {
    /// The version comes from Cargo.toml. When SKILLS_REFERENCE points at a
    /// checkout of the TypeScript CLI (as for the parity harness), the port
    /// must claim the same version as that reference.
    #[test]
    fn version_matches_reference_when_set() {
        assert!(!super::VERSION.is_empty());
        let Some(reference) = std::env::var_os("SKILLS_REFERENCE") else {
            return;
        };
        let pkg = std::fs::read_to_string(std::path::Path::new(&reference).join("package.json"))
            .expect("SKILLS_REFERENCE must contain package.json");
        let v: serde_json::Value = serde_json::from_str(&pkg).unwrap();
        assert_eq!(
            v["version"].as_str().unwrap(),
            super::VERSION,
            "bump rust/Cargo.toml to the reference CLI's version"
        );
    }
}

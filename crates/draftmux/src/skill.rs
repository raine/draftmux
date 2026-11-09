//! `draftmux skill`: prints the agent skill's instructions, and installs the
//! stub skill that loads them, so the instructions always match the installed
//! binary.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use draftmux_pi_profiles::{AddedProfiles, Profile, detect};
use draftmux_settings::tilde_path;

/// The instructions the agent follows, printed by `draftmux skill`.
const BODY: &str = include_str!("skill.md");
/// The SKILL.md that agents load: frontmatter plus a body that runs
/// `draftmux skill`.
const STUB: &str = include_str!("../../../skills/draftmux/SKILL.md");
const NAME: &str = "draftmux";
/// The line of every stub that loads the instructions.
const LOADER: &str = "!`draftmux skill`";

/// Print the instructions of the `draftmux` agent skill.
#[derive(clap::Args, Debug)]
pub struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Install the skill for Claude Code and pi. Never replaces an existing
    /// skill it didn't install.
    Install,
}

/// Returns the process exit code.
pub fn main(args: Args) -> Result<u8> {
    match args.command {
        None => crate::write_output(&mut io::stdout().lock(), BODY.as_bytes()).map(|()| 0),
        Some(Command::Install) => install_main(),
    }
}

fn install_main() -> Result<u8> {
    let home = std::env::home_dir().context("can't find the home directory")?;
    // The same config dir the skill's session locator uses.
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    let added = draftmux_storage::config_dir()
        .and_then(|dir| AddedProfiles::new(&dir).load())
        .unwrap_or_default();
    let profiles = detect(&home, &added);
    let claude = claude.is_dir().then_some(claude);
    if claude.is_none() && profiles.is_empty() {
        anyhow::bail!(
            "found no Claude Code (~/.claude) or pi (~/.pi/agent) config to install into"
        );
    }

    let entries = install(
        claude.as_deref(),
        &profiles,
        &home.join(".agents").join("skills"),
    );
    let mut failed = false;
    for Entry {
        agent,
        path,
        outcome,
    } in entries
    {
        let path = tilde_path(&path, &home).display().to_string();
        match outcome {
            Outcome::Installed => println!("installed the draftmux skill for {agent} at {path}"),
            Outcome::Updated => println!("updated the draftmux skill for {agent} at {path}"),
            Outcome::Present => println!("the draftmux skill for {agent} is already at {path}"),
            Outcome::Taken => println!(
                "skipped {agent}: {path} already exists and draftmux didn't install it; remove it and run this again to replace it"
            ),
            Outcome::AgentsSkill => {
                println!("skipped {agent}: every pi profile already loads {path}")
            }
            Outcome::Failed(err) => {
                failed = true;
                eprintln!("draftmux: installing for {agent} at {path}: {err:#}");
            }
        }
    }
    Ok(if failed { 1 } else { 0 })
}

struct Entry {
    agent: String,
    path: PathBuf,
    outcome: Outcome,
}

#[derive(Debug)]
enum Outcome {
    Installed,
    /// Replaced a stub from an earlier install.
    Updated,
    /// The path already holds exactly what install would create.
    Present,
    /// The path holds something else, which install leaves alone.
    Taken,
    /// pi skipped because `~/.agents/skills/draftmux` exists.
    AgentsSkill,
    Failed(anyhow::Error),
}

/// What to put at a skill path: the stub in a new directory, or a link to
/// another skill directory.
enum Want<'a> {
    Stub,
    Link(&'a Path),
}

/// Installs the stub into `claude` (Claude Code's config dir, if it exists)
/// and each pi profile.
///
/// pi skips a skill whose real path it has already loaded, but warns about
/// two different files with the same name. So every path install creates
/// leads to one `SKILL.md`, and a profile that also lists `~/.claude/skills`
/// in its settings still sees one `draftmux` skill: pi profiles link to
/// Claude Code's skill when there is one. pi reads `agents_skills`
/// (`~/.agents/skills`) in every profile, so a `draftmux` skill there already
/// covers pi, and Claude Code links to it instead of getting its own copy.
fn install(claude: Option<&Path>, profiles: &[Profile], agents_skills: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let agents_skill = agents_skills.join(NAME);
    let agents_link = (!profiles.is_empty() && fs::symlink_metadata(&agents_skill).is_ok())
        .then_some(agents_skill.as_path());
    let claude_skill = claude.map(|dir| dir.join("skills").join(NAME));
    if let Some(path) = &claude_skill {
        let want = match agents_link {
            Some(target) if is_skill(target) => Want::Link(target),
            _ => Want::Stub,
        };
        entries.push(Entry {
            agent: "Claude Code".into(),
            outcome: place(path, want),
            path: path.clone(),
        });
    }

    if let Some(path) = agents_link {
        entries.push(Entry {
            agent: "pi".into(),
            path: path.to_owned(),
            outcome: Outcome::AgentsSkill,
        });
        return entries;
    }
    let link = claude_skill.as_deref().filter(|path| is_skill(path));
    for profile in profiles {
        let path = profile.dir.join("skills").join(NAME);
        entries.push(Entry {
            agent: format!("pi ({})", profile.name),
            outcome: place(&path, link.map_or(Want::Stub, Want::Link)),
            path,
        });
    }
    entries
}

/// Whether `dir` is a skill directory, following links.
fn is_skill(dir: &Path) -> bool {
    dir.join("SKILL.md").is_file()
}

/// Creates `want` at `path` unless something is already there. A stub that
/// install wrote earlier, in any version, is updated to `want`; anything
/// else is left alone.
fn place(path: &Path, want: Want) -> Outcome {
    let ours = stub_text(path).filter(|text| is_stub(text));
    let result = match (&want, fs::symlink_metadata(path)) {
        (Want::Link(target), Ok(_)) if fs::read_link(path).is_ok_and(|l| l == *target) => {
            return Outcome::Present;
        }
        (Want::Stub, Ok(_)) if ours.as_deref() == Some(STUB) => return Outcome::Present,
        (Want::Stub, Ok(_)) if ours.is_some() => fs::write(path.join("SKILL.md"), STUB)
            .with_context(|| format!("writing {}", path.display())),
        // `remove_dir` fails if anything else appeared in the directory.
        (Want::Link(_), Ok(_)) if ours.is_some() => fs::remove_file(path.join("SKILL.md"))
            .and_then(|()| fs::remove_dir(path))
            .map_err(anyhow::Error::from)
            .and_then(|()| create(path, &want)),
        (_, Ok(_)) => return Outcome::Taken,
        (_, Err(e)) if e.kind() == io::ErrorKind::NotFound => {
            return match create(path, &want) {
                Ok(()) => Outcome::Installed,
                Err(err) => Outcome::Failed(err),
            };
        }
        (_, Err(e)) => Err(e.into()),
    };
    match result {
        Ok(()) => Outcome::Updated,
        Err(err) => Outcome::Failed(err),
    }
}

/// The text of `path/SKILL.md` when `path` is a directory, not a link,
/// holding only that regular file, as `create` makes it.
fn stub_text(path: &Path) -> Option<String> {
    let file = path.join("SKILL.md");
    (fs::symlink_metadata(path).ok()?.is_dir()
        && fs::read_dir(path).ok()?.count() == 1
        && fs::symlink_metadata(&file).ok()?.is_file())
    .then(|| fs::read_to_string(&file).ok())
    .flatten()
}

/// Whether `text` is a stub install wrote, in this or any other version:
/// every version names the skill `draftmux` and loads `draftmux skill`.
fn is_stub(text: &str) -> bool {
    let mut lines = text.lines();
    lines.any(|line| line == "name: draftmux") && lines.any(|line| line == LOADER)
}

/// Creates `want` at `path`, failing rather than replacing anything that
/// appeared there since `place` checked.
fn create(path: &Path, want: &Want) -> Result<()> {
    let parent = path.parent().expect("skill paths have a parent");
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    match want {
        Want::Link(target) => symlink(target, path)?,
        Want::Stub => {
            fs::create_dir(path)?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path.join("SKILL.md"))?
                .write_all(STUB.as_bytes())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(home: &Path, name: &str) -> Profile {
        let dir = home.join(format!(".{name}")).join("agent");
        fs::create_dir_all(&dir).unwrap();
        Profile {
            dir,
            name: name.into(),
        }
    }

    fn outcomes(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| format!("{}: {:?}", e.agent, e.outcome))
            .collect()
    }

    #[test]
    fn stub_loads_the_body() {
        assert!(STUB.starts_with("---\nname: draftmux\n"));
        assert!(STUB.contains("disable-model-invocation: true\n"));
        assert!(STUB.contains(&format!("\n{LOADER}\n")));
        // Install must keep recognizing the stubs it wrote before.
        assert!(is_stub(STUB));
        // Claude Code substitutes these in command output too.
        assert!(!BODY.contains("!`") && !BODY.contains("$ARGUMENTS"));
    }

    #[test]
    fn installs_claude_stub_and_links_pi_to_it() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let claude = home.join(".claude");
        fs::create_dir(&claude).unwrap();
        let profiles = [profile(home, "pi"), profile(home, "pi-work")];
        let agents = home.join(".agents/skills");

        let entries = install(Some(&claude), &profiles, &agents);
        assert_eq!(
            outcomes(&entries),
            [
                "Claude Code: Installed",
                "pi (pi): Installed",
                "pi (pi-work): Installed"
            ]
        );
        let claude_skill = claude.join("skills/draftmux");
        assert_eq!(
            fs::read_to_string(claude_skill.join("SKILL.md")).unwrap(),
            STUB
        );
        for profile in &profiles {
            let link = profile.dir.join("skills/draftmux");
            assert_eq!(fs::read_link(&link).unwrap(), claude_skill);
        }

        let again = install(Some(&claude), &profiles, &agents);
        assert_eq!(
            outcomes(&again),
            [
                "Claude Code: Present",
                "pi (pi): Present",
                "pi (pi-work): Present"
            ]
        );
    }

    #[test]
    fn pi_alone_gets_its_own_stub() {
        let home = tempfile::tempdir().unwrap();
        let profiles = [profile(home.path(), "pi")];
        let entries = install(None, &profiles, &home.path().join(".agents/skills"));
        assert_eq!(outcomes(&entries), ["pi (pi): Installed"]);
        let file = profiles[0].dir.join("skills/draftmux/SKILL.md");
        assert_eq!(fs::read_to_string(file).unwrap(), STUB);
    }

    #[test]
    fn leaves_existing_skills_alone() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let claude = home.join(".claude");
        let skills = claude.join("skills");
        fs::create_dir_all(&skills).unwrap();
        // A link to a checkout the user made by hand.
        let checkout = home.join("checkout/skills/draftmux");
        fs::create_dir_all(&checkout).unwrap();
        symlink(&checkout, skills.join("draftmux")).unwrap();
        // An edited copy and a dangling link in the pi profiles.
        let edited = profile(home, "pi");
        let edited_skill = edited.dir.join("skills/draftmux");
        fs::create_dir_all(&edited_skill).unwrap();
        fs::write(edited_skill.join("SKILL.md"), "mine").unwrap();
        let dangling = profile(home, "pi-work");
        fs::create_dir_all(dangling.dir.join("skills")).unwrap();
        symlink(home.join("gone"), dangling.dir.join("skills/draftmux")).unwrap();

        let entries = install(
            Some(&claude),
            &[edited, dangling.clone()],
            &home.join(".agents/skills"),
        );
        assert_eq!(
            outcomes(&entries),
            [
                "Claude Code: Taken",
                "pi (pi): Taken",
                "pi (pi-work): Taken"
            ]
        );
        assert_eq!(fs::read_link(skills.join("draftmux")).unwrap(), checkout);
        assert_eq!(
            fs::read_to_string(edited_skill.join("SKILL.md")).unwrap(),
            "mine"
        );
        assert_eq!(
            fs::read_link(dangling.dir.join("skills/draftmux")).unwrap(),
            home.join("gone")
        );
    }

    #[test]
    fn agents_skill_covers_pi() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let claude = home.join(".claude");
        fs::create_dir(&claude).unwrap();
        let agents = home.join(".agents/skills");
        fs::create_dir_all(agents.join("draftmux")).unwrap();
        fs::write(agents.join("draftmux/SKILL.md"), "mine").unwrap();
        let profiles = [profile(home, "pi")];

        let entries = install(Some(&claude), &profiles, &agents);
        assert_eq!(
            outcomes(&entries),
            ["Claude Code: Installed", "pi: AgentsSkill"]
        );
        assert_eq!(
            fs::read_link(claude.join("skills/draftmux")).unwrap(),
            agents.join("draftmux")
        );
        assert!(fs::symlink_metadata(profiles[0].dir.join("skills/draftmux")).is_err());
    }

    #[test]
    fn links_an_earlier_pi_stub_once_claude_has_the_skill() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let profiles = [profile(home, "pi")];
        let agents = home.join(".agents/skills");
        install(None, &profiles, &agents);

        let claude = home.join(".claude");
        fs::create_dir(&claude).unwrap();
        let entries = install(Some(&claude), &profiles, &agents);
        assert_eq!(
            outcomes(&entries),
            ["Claude Code: Installed", "pi (pi): Updated"]
        );
        assert_eq!(
            fs::read_link(profiles[0].dir.join("skills/draftmux")).unwrap(),
            claude.join("skills/draftmux")
        );
    }

    #[test]
    fn does_not_link_pi_to_a_directory_without_a_skill() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let claude = home.join(".claude");
        fs::create_dir_all(claude.join("skills/draftmux")).unwrap();
        let profiles = [profile(home, "pi")];

        let entries = install(Some(&claude), &profiles, &home.join(".agents/skills"));
        assert_eq!(
            outcomes(&entries),
            ["Claude Code: Taken", "pi (pi): Installed"]
        );
        let file = profiles[0].dir.join("skills/draftmux/SKILL.md");
        assert_eq!(fs::read_to_string(file).unwrap(), STUB);
    }

    #[test]
    fn updates_stubs_from_earlier_versions_only() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let claude = home.join(".claude");
        let skill = claude.join("skills/draftmux");
        fs::create_dir_all(&skill).unwrap();
        let old = "---\nname: draftmux\ndescription: old\n---\n\n!`draftmux skill`\n";
        fs::write(skill.join("SKILL.md"), old).unwrap();
        // The full skill copied by hand, without the loader line.
        let copied = profile(home, "pi");
        let copied_skill = copied.dir.join("skills/draftmux");
        fs::create_dir_all(&copied_skill).unwrap();
        let full = format!("---\nname: draftmux\n---\n\n{BODY}");
        fs::write(copied_skill.join("SKILL.md"), &full).unwrap();
        // An earlier stub with something else added beside it.
        let extra = profile(home, "pi-work");
        let extra_skill = extra.dir.join("skills/draftmux");
        fs::create_dir_all(&extra_skill).unwrap();
        fs::write(extra_skill.join("SKILL.md"), old).unwrap();
        fs::write(extra_skill.join("notes.md"), "mine").unwrap();

        let entries = install(
            Some(&claude),
            &[copied, extra],
            &home.join(".agents/skills"),
        );
        assert_eq!(
            outcomes(&entries),
            [
                "Claude Code: Updated",
                "pi (pi): Taken",
                "pi (pi-work): Taken"
            ]
        );
        assert_eq!(fs::read_to_string(skill.join("SKILL.md")).unwrap(), STUB);
        assert_eq!(
            fs::read_to_string(copied_skill.join("SKILL.md")).unwrap(),
            full
        );
        assert_eq!(
            fs::read_to_string(extra_skill.join("SKILL.md")).unwrap(),
            old
        );
    }
}

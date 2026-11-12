//! `holosmith rollback`: go back one release (HS-079, spec item 10's Commands table, ADR-0083).
//!
//! Item 10's closing sentence is two verbs — "in v1 these steps are implemented by `holosmith upgrade <binary>` and
//! `holosmith rollback`" — and HS-078 built the first. This is the second: the same switch-in-reverse a promotion
//! performs by itself when a candidate does not answer, reachable **on demand**, with the target chosen from the tree
//! rather than from what a promotion happened to capture, and with the schema refusal read *before* anything is
//! touched.
//!
//! The order is the acceptance's own:
//!
//! 1. **the target** — the release one step back from the one `current` names ([`crate::release::Release::older_than`],
//!    or `--to TAG`), read through its own `version --json` so the poll knows what version it is waiting for;
//! 2. **the schema** — the target's schema against what the database records, refused here because the unit's
//!    `ExecStartPre=doctor --quick` would refuse it later, after a drain and a restart (ADR-0047, ADR-0082's recorded
//!    limit);
//! 3. **the unit** — that it exists at all, and that its `ExecStart` goes through `current`, which is the only reason a
//!    switch takes effect at the restart;
//! 4. **the drain** — the same client a promotion uses, except that this one **joins** a drain already in progress
//!    instead of refusing it (a machine left draining is exactly the machine that needs rolling back);
//! 5. **the switch** — `Release::switch`, the same atomic rename of a relative symlink;
//! 6. **the restart and the poll** — `promote::restart_and_poll`, the same `systemctl restart` and the same
//!    `/api/health` question for a process **younger than the restart**;
//! 7. **the ending** — one line naming the release that is serving, or a loud sentence naming `systemctl status`,
//!    `journalctl`, the release now `current` and the way back forward.
//!
//! # What this verb does not do
//!
//! No backup: a rollback changes no database, and the promotion that preceded it took the backup. No database
//! restore: expand/contract is what makes switching `current` back safe (item 10's own promise, ADR-0047). No row and
//! no container: the drain stops the idle sessions with its own reason and the next boot resumes them
//! ([`crate::reconcile`]), so "sessions survive" is a claim about not adding a second path. No review acknowledgement:
//! item 12's review is a pre-promotion gate and this is the emergency brake. And no second switch when the poll
//! fails — a rollback that guesses twice is a rollback nobody can reason about.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use crate::config::Config;
use crate::deploy::{self, Target};
use crate::promote::{self, DrainAnswer, Steps};
use crate::release::{self, Release};
use crate::version::VersionInfo;

/// What the command line asked for.
#[derive(Debug, Clone)]
pub struct Options {
    /// The release to switch to, by tag. `None` means the release the live one replaced.
    pub to: Option<String>,
    /// How long the drain may wait for the running turns (the flag's, default `10m`).
    pub wait: Duration,
    /// How long the target has to answer after the restart: [`promote::WINDOW`] for the command, and a parameter for
    /// the reason the promotion's is one — a test that had to wait the spec's twenty seconds to prove the poll gives
    /// up would be a test nobody runs.
    pub window: Duration,
}

/// The whole rollback: what it was asked for, and the exit code its endings carry.
pub fn run(config: &Config, target: &Target, options: &Options, steps: &dyn Steps) -> ExitCode {
    let tools = deploy::Tools::default();
    match execute(config, target, options, steps, &tools) {
        Ok(()) => ExitCode::SUCCESS,
        Err(said) => {
            eprintln!("holosmith: rollback: {said}");
            ExitCode::FAILURE
        }
    }
}

/// [`run`] with the programs as a parameter, so a test can name its own `systemctl`.
pub fn execute(
    config: &Config,
    target: &Target,
    options: &Options,
    steps: &dyn Steps,
    tools: &deploy::Tools,
) -> Result<(), String> {
    let release = Release::of(target);
    let current = live(&release)?;
    let tag = choose(&release, &current, options.to.as_deref())?;
    let binary = release.binary(&tag);
    // The target's own three facts, from the target: the version the poll looks for, the tag it was built from and the
    // schema it understands. A release directory is not asked what it is — the binary inside it is.
    let info = promote::ask_version(&binary).map_err(|said| {
        format!(
            "{said} — a rollback switches `current` to releases/{tag}, so that directory has to hold a `holosmith` \
             this machine can run. Nothing was drained and nothing was switched"
        )
    })?;
    // The refusal the acceptance names, before anything is asked of the machine: an older release cannot read a
    // database a newer one wrote, and the unit's `doctor --quick` would refuse it *after* a drain and a restart.
    promote::refuse_older_schema(
        config,
        &format!("releases/{tag} ({})", binary.display()),
        info.schema_version,
        "`--to` another release in this tree, or a database this one understands",
    )?;
    println!(
        "rollback: target: releases/{tag} ({} ({}), schema {}), from releases/{current}",
        info.version, info.git_tag, info.schema_version
    );

    // 3. The unit. A rollback restarts a service, so a machine without a unit is one that has not been deployed; and a
    //    unit whose `ExecStart` does not go through `current` is one the switch cannot reach (`write_unit` compares the
    //    bytes, so a promoted machine writes nothing here).
    let unit = target.unit_path();
    if !unit.is_file() {
        return Err(format!(
            "{} is not there: a rollback restarts the unit systemd runs, so this machine has to have been deployed \
             (`holosmith server deploy` writes it). Nothing was drained and nothing was switched",
            unit.display()
        ));
    }
    promote::write_unit(target, config, &release, "rollback")
        .map_err(|said| format!("{said} — nothing was drained and nothing was switched"))?;

    // 4. The drain. From here on the machine is draining until it is restarted, so every refusal below says so
    //    (`promote::draining`, one clause for both verbs).
    match steps.drain(&target.admin_dir(), options.wait) {
        DrainAnswer::Drained => println!("rollback: drain: drained"),
        // A promotion refuses this (`DrainAnswer::Already`); a rollback joined the drain instead, because a machine
        // left draining is a machine this verb exists for (ADR-0083 D4).
        DrainAnswer::Already => println!(
            "rollback: drain: the machine was already draining, and this rollback joined that drain rather than \
             waiting for a promotion that will not come"
        ),
        DrainAnswer::Expired => {
            return Err(promote::draining(format!(
                "the drain's wait ran out with a turn still running, so nothing was switched: `systemctl restart {}` \
                 clears the drain, and releases/{current} is still live",
                deploy::UNIT_NAME
            )));
        }
        DrainAnswer::Failed(sentence) => {
            return Err(format!(
                "the drain did not happen, so nothing was switched: {sentence}"
            ));
        }
    }

    // 5. The switch: one atomic rename of a relative symlink, so systemd's next (re)start picks it up.
    release.switch(&tag).map_err(promote::draining)?;
    println!("rollback: current: -> releases/{tag} (was releases/{current})");

    // 6. The restart and the poll -- the same two steps a promotion runs, through the same function.
    let attempt = promote::restart_and_poll(
        config,
        steps,
        tools,
        Some(info.version.as_str()),
        options.window,
        "rollback",
    );
    if let Some(answer) = attempt.answered {
        println!(
            "rollback: health: {} answered {:.1}s after the restart",
            answer.version,
            attempt.started.elapsed().as_secs_f32()
        );
        println!(
            "rollback: live: releases/{tag} ({}, {}), serving",
            info.version, info.git_tag
        );
        return Ok(());
    }

    // 7. The loud ending. The link stays where the rollback put it: a second automatic switch would be a second guess,
    //    and what an operator needs here is the truth plus the commands that explain it.
    Err(not_live(&tag, &current, &info, &attempt))
}

/// The sentence a rollback whose own poll failed ends with: the state it left, and the next step.
///
/// The acceptance's fourth bullet is "says so loudly and leaves the operator a stated next step", and there are three
/// that are true: the journal (`systemctl status`/`journalctl`, which is where the service's own reason is), the
/// release now `current` (named, so the operator knows what they are looking at), and `--to <the release it came
/// from>` (which is *newer* than the live one now, and `--to` accepts it — going forward again is the same switch).
fn not_live(tag: &str, was: &str, info: &VersionInfo, attempt: &promote::Attempt) -> String {
    let restart = match &attempt.restarted {
        Ok(()) => String::new(),
        Err(sentence) => format!("The restart itself failed first: {sentence}. "),
    };
    format!(
        "{restart}releases/{tag} did not answer `/api/health` as a process younger than the restart, so this rollback \
         is not confirmed live — and `current` still names it. `systemctl status {unit}` and `journalctl -u {unit}` \
         say why the service will not start; `holosmith rollback --to {was}` switches back to the release that was \
         live before this rollback, if that is the one you need. The database was not restored — the migration rule \
         (expand/contract) is what makes switching `current` safe — and if releases/{tag} (schema {schema}) is older \
         than the database, its own `doctor --quick` refuses the database before `ExecStart` (ADR-0047): the release \
         to run is then one this database's schema understands",
        unit = deploy::UNIT_NAME,
        schema = info.schema_version
    )
}

/// The tag `current` names, or why this tree cannot say which release is live.
///
/// Three arms, and each is a tree a rollback has nowhere to go from: no `current` at all (never promoted), a `current`
/// that is not a symlink (nothing a promotion wrote), and a link that names something outside `releases/<tag>`. The
/// first is `promote::previous_release`'s seating rule seen from the other side — that function *creates* the first
/// release and this one refuses to guess at it.
fn live(release: &Release) -> Result<String, String> {
    let current = release.current();
    match std::fs::symlink_metadata(&current) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let link = release.link().ok_or_else(|| {
                format!(
                    "{} could not be read as a symlink: a rollback switches `current` between `releases/<tag>` \
                     directories and has to know what it names. Nothing was drained and nothing was switched",
                    current.display()
                )
            })?;
            release.tag_of(&link).ok_or_else(|| {
                format!(
                    "{} names {}, which is not one of this tree's releases: a rollback switches `current` between \
                     `releases/<tag>` directories, and cannot say which release is behind a link to somewhere else. \
                     Nothing was drained and nothing was switched",
                    current.display(),
                    link.display()
                )
            })
        }
        Ok(_) => Err(format!(
            "{} is not a symlink: a rollback switches the `current` link a promotion writes (item 10's \
             `releases/<tag>/` and `current`). Nothing was drained and nothing was switched",
            current.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "{} is not there: a rollback switches the `current` symlink back one release, and this machine has never \
             promoted (`holosmith upgrade <binary>` writes the first release). Nothing was drained and nothing was \
             switched",
            current.display()
        )),
        Err(error) => Err(format!(
            "{} could not be read ({error}): a rollback has to know which release is live to go one back from it. \
             Nothing was drained and nothing was switched",
            current.display()
        )),
    }
}

/// Which release to switch to: `--to`'s tag, or the one behind [`live`] in the tree's order.
fn choose(release: &Release, current: &str, to: Option<&str>) -> Result<String, String> {
    let history = release.history();
    let Some(wanted) = to else {
        if !history.iter().any(|known| known == current) {
            return Err(format!(
                "`current` names releases/{current}, and this tree holds no release by that name with a `holosmith` \
                 in it ({}): the order to go one back in is built from the directories that do, and none of them is \
                 the live one. Nothing was drained and nothing was switched",
                listing(&release.releases(), &history)
            ));
        }
        return release.older_than(current).ok_or_else(|| {
            format!(
                "releases/{current} is the oldest release this tree holds ({}), so there is no release behind it to \
                 go back to: `--to TAG` names one explicitly. Nothing was drained and nothing was switched",
                listing(&release.releases(), &history)
            )
        });
    };
    if let Some(problem) = release::tag_problem(wanted) {
        return Err(format!(
            "`--to {wanted}` cannot name a release: {problem}. Nothing was drained and nothing was switched"
        ));
    }
    if !history.iter().any(|known| known == wanted) {
        return Err(format!(
            "`--to {wanted}` names no release in this tree: {} holds {}. Nothing was drained and nothing was switched",
            release.releases().display(),
            listing(&release.releases(), &history)
        ));
    }
    if wanted == current {
        return Err(format!(
            "`--to {wanted}` is the release `current` already names: a rollback switches `current` away from what it \
             names, so this would be a restart with no release changed (`systemctl restart {}` is that command). \
             Nothing was drained and nothing was switched",
            deploy::UNIT_NAME
        ));
    }
    Ok(wanted.to_string())
}

/// The releases this tree holds, as one clause — so a refusal names what the operator could have asked for.
fn listing(directory: &Path, history: &[String]) -> String {
    if history.is_empty() {
        return format!("{} (no release at all)", directory.display());
    }
    format!("{} holds {}", directory.display(), history.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A temporary tree, removed on drop: four tests over real directories' mtimes and one symlink.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("holosmith-rollback-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a temporary directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A tree with the given releases, written oldest first, and `current` on the newest.
    fn world(tags: &[&str]) -> (TempDir, Target, Release) {
        let root = TempDir::new("unit");
        let target = Target::resolve(Some(root.0.clone()));
        let release = Release::of(&target);
        std::fs::create_dir_all(release.releases()).expect("a release tree");
        for (at, tag) in tags.iter().enumerate() {
            let dir = release.dir_of(tag);
            std::fs::create_dir_all(&dir).expect("a release directory");
            std::fs::write(dir.join(release::BINARY), b"not a binary")
                .expect("a placeholder binary");
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + at as u64);
            std::fs::File::open(&dir)
                .expect("the directory is open")
                .set_modified(when)
                .expect("the mtime is set");
        }
        if let Some(newest) = tags.last() {
            release.switch(newest).expect("the link is written");
        }
        (root, target, release)
    }

    #[test]
    fn the_default_target_is_the_release_behind_the_live_one() {
        let (_root, _target, release) = world(&["one", "two", "three"]);
        assert_eq!(live(&release).as_deref(), Ok("three"));
        assert_eq!(
            choose(&release, "three", None).as_deref(),
            Ok("two"),
            "the newest release behind the live one"
        );
        assert_eq!(choose(&release, "two", None).as_deref(), Ok("one"));
        // The oldest has nothing behind it, and the sentence says so rather than switching anywhere.
        let refused =
            choose(&release, "one", None).expect_err("the oldest has no previous release");
        assert!(
            refused.contains("oldest release this tree holds"),
            "{refused}"
        );
    }

    #[test]
    fn a_tree_with_one_release_or_none_live_is_refused() {
        let (_root, _target, one) = world(&["only"]);
        let refused = choose(&one, "only", None).expect_err("one release has no previous");
        assert!(
            refused.contains("oldest release this tree holds"),
            "{refused}"
        );

        // A tree with no `current` at all: never promoted, and the sentence names the command that writes the first.
        let (_root, _target, empty) = world(&[]);
        let refused = live(&empty).expect_err("no current");
        assert!(refused.contains("has never promoted"), "{refused}");

        // A `current` that is a real directory rather than a link is not one a promotion wrote.
        std::fs::create_dir_all(empty.current()).expect("a directory named current");
        let refused = live(&empty).expect_err("current is not a link");
        assert!(refused.contains("is not a symlink"), "{refused}");
    }

    #[test]
    fn a_named_release_has_to_be_in_the_tree_and_not_the_live_one() {
        let (_root, _target, release) = world(&["one", "two"]);
        assert_eq!(choose(&release, "two", Some("one")).as_deref(), Ok("one"));
        // Older is the default; a name is how an operator asks for any other, including one ahead again.
        let refused =
            choose(&release, "one", Some("one")).expect_err("the live release is not a rollback");
        assert!(refused.contains("already names"), "{refused}");
        let refused = choose(&release, "two", Some("three")).expect_err("not in the tree");
        assert!(
            refused.contains("names no release in this tree"),
            "{refused}"
        );
        assert!(refused.contains("holds two, one"), "{refused}");
        // A path is not a tag: one rule (`tag_problem`), not a second spelling of it.
        let refused =
            choose(&release, "two", Some("releases/one")).expect_err("a path is not a tag");
        assert!(refused.contains("cannot name a release"), "{refused}");
    }
}

//! The daily project backup: one git bundle per project per day, on the host.
//!
//! Spec item 1's `project_backup` setting, and the "Known risks" line that motivates it: several
//! sessions share one project repository, so an agent that damages it damages every session's base.
//! A bundle is the whole repository in one file — every branch, every tag, `--all` — which is what makes
//! it a recovery point rather than a working copy.
//!
//! ```text
//! git bundle create <data>/backups/<project>-<date>.bundle --all
//! git bundle verify <data>/backups/<project>-<date>.bundle
//! ```
//!
//! Three rules, and each is the story's:
//!
//! - **Once per project per day.** The bundle's own name carries the date, so "has today's been taken"
//!   is a file that exists — no state, no column, no marker file. A second session on the same day finds
//!   it and does nothing.
//! - **Verified before it is kept.** The bundle is written to a temporary name, verified with
//!   `git bundle verify`, and only then renamed into place. A bundle git cannot read is deleted rather
//!   than kept, because a recovery point that cannot be restored is worse than none: it is a file an
//!   operator would trust. The temporary name is **per attempt**, not per day: two creates on one project
//!   run at once on this runtime, and a shared temporary name would let the second truncate the file the
//!   first is about to rename — which would put a rewritten file under the name that verified.
//! - **The last seven are kept.** One week of days per project, pruned by name — the date is the sort
//!   key, so no file has to be read to know which are oldest.
//!
//! The module is synchronous and `std`-only, like [`crate::worktree`]: it runs one program and reads one
//! answer, and the caller runs it on a blocking task. Nothing here logs — a failure answers a
//! [`BackupError`] whose sentence goes to the log at the one place that can also decide it must not
//! block anything.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// The directory under the data root that holds every project's bundles: `<data>/backups`.
pub const BACKUPS: &str = "backups";

/// How many days of bundles one project keeps: a week.
pub const KEEP: usize = 7;

/// How many database snapshots one machine keeps: spec item 10 step 4's "keep five".
///
/// A second number beside [`KEEP`] because they are two different rotations over **one** directory: a project's
/// bundles are one per project per day (a week of days), while a promotion's snapshots are one per promotion
/// (the last five promotions), and a run that kept one number for both would silently shorten a week to five
/// days or a promotion history to seven.
pub const KEEP_SNAPSHOTS: usize = 5;

/// What every database snapshot is named: `app-<ts>.db`.
///
/// `app` is the database's own name ([`crate::db`] opens `<data>/app.db`), so the snapshot says what it is a
/// snapshot *of* without a second suffix to read.
pub const SNAPSHOT: &str = "app";

/// How many bundles this process has written. Only used to give each attempt its own temporary name, so
/// two creates that race on one project cannot truncate each other's file.
static ATTEMPT: AtomicU64 = AtomicU64::new(0);

/// How many seconds are in a day, which is what turns an instant into a calendar day.
const DAY: i64 = 86_400;

/// Why a project's bundle could not be taken.
///
/// Two answers, and the difference is the operator's: git refused, or the filesystem did. Both are
/// logged and neither stops a session — a backup is a safety net, and a net that is broken is not a
/// reason to refuse to work.
#[derive(Debug)]
pub enum BackupError {
    /// `git bundle` exited non-zero, with git's own words.
    Git(String),
    /// A directory could not be created, a file could not be renamed or removed.
    Files(String),
    /// The database itself could not be written by the caller's `VACUUM INTO`.
    Write(String),
}

impl fmt::Display for BackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(words) => write!(formatter, "git: {words}"),
            Self::Files(words) | Self::Write(words) => write!(formatter, "{words}"),
        }
    }
}

/// Neither variant wraps another error: git's words are text and a filesystem error is rendered into
/// one, so there is nothing to walk to. The impl exists because [`crate::worktree::GitError`] has one and
/// a caller that boxes a backup failure should not have to special-case it.
impl std::error::Error for BackupError {}

/// The backups of one machine: the directory they live in, and the day they are named after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backups {
    /// `<data_root>/backups`.
    pub root: PathBuf,
}

impl Backups {
    /// The backups of the machine whose data root is `data_root`.
    ///
    /// Every path this module hands git is absolute, and it has to be: `git -C <repository>` resolves a
    /// relative bundle path against the *repository*, so a relative `root` would write the bundle inside
    /// the checkout and then fail to rename it (ADR-0029 measured the same trap for worktree paths). The
    /// config layer refuses a relative `data_root`, which is what makes the invariant hold.
    pub fn new(data_root: &Path) -> Self {
        Self {
            root: data_root.join(BACKUPS),
        }
    }

    /// Take today's bundle of `repository`, unless it is already there.
    ///
    /// `now` is the instant the day is computed from, passed in rather than read here for the reason
    /// every other clock in this crate is (ADR-0009): a test writes the day it means instead of racing
    /// midnight.
    ///
    /// `Ok(None)` is "today's bundle already existed" — the ordinary answer for every session after the
    /// first on a day, and not a failure. `Err` means today's bundle is not in place, **or** that the
    /// retention rule could not be applied: two operations with two outcomes, reported as one because
    /// both leave the directory not the one this machine promises. The caller logs the error's sentence,
    /// which names which of the two it was.
    pub fn take(
        &self,
        repository: &Path,
        project: &str,
        now: i64,
    ) -> Result<Option<PathBuf>, BackupError> {
        let bundle = self.root.join(format!("{project}-{}.bundle", day(now)));
        if bundle.exists() {
            // The retention rule is applied on this path too, and not only where a bundle is written: it
            // is one `read_dir`, and it is what makes a prune that failed yesterday repair itself today
            // instead of staying stuck until a new day arrives.
            self.prune(project)?;
            return Ok(None);
        }
        std::fs::create_dir_all(&self.root).map_err(|error| {
            BackupError::Files(format!("creating {}: {error}", self.root.display()))
        })?;

        // Written aside and renamed into place, so the name that exists is always a bundle that
        // verified: a half-written file under the final name would be a recovery point an operator
        // would trust and git would refuse. The name is per **attempt**, because two creates on one
        // project can run at once: a name shared by the day would let the second `git bundle create`
        // truncate the file the first is about to rename, and the name that verified would then hold a
        // file rewritten after the check.
        let partial = bundle.with_extension(format!(
            "bundle.{}.{}.tmp",
            std::process::id(),
            ATTEMPT.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(error) = git(repository, &["bundle", "create", &path(&partial), "--all"]) {
            let _ = std::fs::remove_file(&partial);
            return Err(error);
        }
        self.keep(repository, &partial, &bundle)?;
        self.prune(project)?;
        Ok(Some(bundle))
    }

    /// Verify the bundle written aside and, only if git can read it, rename it into place.
    ///
    /// A corrupt bundle is deleted rather than kept (the story's fourth line): a file that cannot be
    /// restored must not sit in the directory looking like one that can. Split out from [`Self::take`]
    /// so that branch has a test — nothing else in this module can make git produce an unreadable bundle
    /// from a readable repository.
    fn keep(&self, repository: &Path, partial: &Path, bundle: &Path) -> Result<(), BackupError> {
        if let Err(error) = git(repository, &["bundle", "verify", &path(partial)]) {
            let _ = std::fs::remove_file(partial);
            return Err(error);
        }
        std::fs::rename(partial, bundle).map_err(|error| {
            let _ = std::fs::remove_file(partial);
            BackupError::Files(format!("renaming {}: {error}", partial.display()))
        })
    }

    /// Delete all but the newest [`KEEP`] bundles of `project`.
    ///
    /// By name, because the name carries the day and a date sorts as text: no bundle is opened to decide
    /// which are oldest, which matters because the point of the prune is to be cheap on a machine that
    /// is already short of space.
    pub fn prune(&self, project: &str) -> Result<(), BackupError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            // Nothing to prune: a machine whose backups directory has never been made has none.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(BackupError::Files(format!(
                    "reading {}: {error}",
                    self.root.display()
                )));
            }
        };
        let prefix = format!("{project}-");
        let mut bundles: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| is_bundle_of(&entry.file_name().to_string_lossy(), &prefix))
            .map(|entry| entry.path())
            .collect();
        // The name is the sort key, and every name in this set is `<prefix><YYYY-MM-DD>.bundle`.
        bundles.sort();
        let excess = bundles.len().saturating_sub(KEEP);
        for bundle in bundles.into_iter().take(excess) {
            std::fs::remove_file(&bundle).map_err(|error| {
                BackupError::Files(format!("removing {}: {error}", bundle.display()))
            })?;
        }
        Ok(())
    }
}

/// Whether `name` is one of `prefix`'s bundles: the prefix, then a ten-character day, then `.bundle`.
///
/// The length check is what keeps two projects apart when one's name is a prefix of the other's
/// (`holosmith` and `holosmith2`): only the exact `<prefix><day>.bundle` shape is a bundle of this
/// project, and the temporary name a bundle is written under is not one of them.
fn is_bundle_of(name: &str, prefix: &str) -> bool {
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    let Some(day) = rest.strip_suffix(".bundle") else {
        return false;
    };
    day.len() == 10
        && day.as_bytes()[4] == b'-'
        && day.as_bytes()[7] == b'-'
        && day
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

/// The calendar day `now` falls in, as `YYYY-MM-DD`, in UTC.
///
/// UTC rather than local time: the day a bundle is named after has to be one value on a machine whose
/// clock and whose operator may be in different zones, and "the machine's own zone" is a setting this
/// system does not have. The operator reads the date as "the last seven days", which is what the prune
/// keeps either way.
fn day(now: i64) -> String {
    let (year, month, date) = civil_from_days(now.div_euclid(DAY));
    format!("{year:04}-{month:02}-{date:02}")
}

/// The proleptic Gregorian date of a count of days since 1970-01-01 (Howard Hinnant's `civil_from_days`,
/// which is the standard integer-only answer and needs no dependency).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // The era is a 400-year cycle, shifted so that the leap day is at the end of the year.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as u32, date as u32)
}

/// The database snapshots one machine keeps: the directory, the name, and the rotation (, spec item 10
/// step 4).
///
/// One type beside [`Backups`] because they are two rotations over one directory (`<data>/backups`): a project's
/// bundles are `<project>-<day>.bundle` and keep a week of days, while a promotion's snapshot is `app-<ts>.db` and
/// keeps the last five promotions. The two rules share the **name is the sort key** argument and nothing else, which
/// is why they are two types in one module rather than one type with two modes.
///
/// The vacuum itself is the caller's: this module is `std`-only ([`crate::db`] owns SQL), so [`take`] is handed one
/// function that writes a database to a path it is given. That is the seam a test uses to prove the *rotation* and
/// the rename without a database, and the seam the promotion uses to hand it `db::Reader::snapshot`.
///
/// [`take`]: Snapshots::take
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshots {
    /// `<data_root>/backups`.
    pub root: PathBuf,
}

impl Snapshots {
    /// The snapshots of the machine whose data root is `data_root`.
    pub fn new(data_root: &Path) -> Self {
        Self {
            root: data_root.join(BACKUPS),
        }
    }

    /// The name a snapshot taken at `now` takes: `<data>/backups/app-<ts>.db`.
    ///
    /// The stamp is UTC and fixed-width (`YYYYMMDDTHHMMSS`), so the name **is** the sort key: the prune reads no
    /// database and opens no file to know which snapshot is oldest, which is the same argument
    /// [`Backups::prune`] makes about a bundle's day.
    pub fn path(&self, now: i64) -> PathBuf {
        self.root.join(format!("{SNAPSHOT}-{}.db", stamp(now)))
    }

    /// Take a snapshot of the database with `vacuum`, prune to [`KEEP_SNAPSHOTS`], and answer its path.
    ///
    /// Written aside and renamed into place, so the name that exists under `app-<ts>.db` is always a whole database:
    /// a half-written file would be a recovery point an operator would trust. A second promotion inside the same
    /// second replaces the first one's snapshot (a rename over an existing name is atomic), because two snapshots of
    /// one instant differ in nothing an operator can use and the **name** is what the rotation sorts by.
    pub fn take(
        &self,
        now: i64,
        vacuum: &dyn Fn(&Path) -> Result<(), String>,
    ) -> Result<PathBuf, BackupError> {
        std::fs::create_dir_all(&self.root).map_err(|error| {
            BackupError::Files(format!("creating {}: {error}", self.root.display()))
        })?;
        let snapshot = self.path(now);
        let partial = snapshot.with_extension(format!("db.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&partial);
        if let Err(sentence) = vacuum(&partial) {
            let _ = std::fs::remove_file(&partial);
            return Err(BackupError::Write(sentence));
        }
        std::fs::rename(&partial, &snapshot).map_err(|error| {
            let _ = std::fs::remove_file(&partial);
            BackupError::Files(format!(
                "renaming {} to {}: {error}",
                partial.display(),
                snapshot.display()
            ))
        })?;
        self.prune()?;
        Ok(snapshot)
    }

    /// Delete all but the newest [`KEEP_SNAPSHOTS`] snapshots.
    ///
    /// By name, and only the names this module writes: a `.tmp` file of a run that died is not a snapshot, so it is
    /// neither counted nor deleted here — the next [`take`](Self::take) removes its own, and an operator's own file
    /// in this directory is not this module's to remove.
    pub fn prune(&self) -> Result<(), BackupError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(BackupError::Files(format!(
                    "reading {}: {error}",
                    self.root.display()
                )));
            }
        };
        let mut snapshots: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| is_snapshot_of(&entry.file_name().to_string_lossy()))
            .map(|entry| entry.path())
            .collect();
        snapshots.sort();
        let excess = snapshots.len().saturating_sub(KEEP_SNAPSHOTS);
        for snapshot in snapshots.into_iter().take(excess) {
            std::fs::remove_file(&snapshot).map_err(|error| {
                BackupError::Files(format!("removing {}: {error}", snapshot.display()))
            })?;
        }
        Ok(())
    }
}

/// Whether `name` is one of this module's snapshots: `app-`, a fifteen-character stamp, `.db`.
///
/// The shape check is what keeps a `.tmp` file, an operator's own `app.db` and a project bundle apart: only
/// `app-<YYYYMMDDTHHMMSS>.db` is counted by the rotation, and everything else in the directory is left alone.
fn is_snapshot_of(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(SNAPSHOT)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    let Some(day) = rest.strip_suffix(".db") else {
        return false;
    };
    day.len() == 15
        && day.as_bytes()[8] == b'T'
        && day
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 8 || byte.is_ascii_digit())
}

/// The instant `now` as `YYYYMMDDTHHMMSS`, in UTC.
///
/// The day half is [`day`]'s arithmetic, split out so the two names cannot disagree about which day an instant falls
/// in; the time half is the remainder of the day, because UTC has no leap seconds in the calendar this build uses.
fn stamp(now: i64) -> String {
    let (year, month, date) = civil_from_days(now.div_euclid(DAY));
    let second_of_day = now.rem_euclid(DAY);
    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    let second = second_of_day % 60;
    format!("{year:04}{month:02}{date:02}T{hour:02}{minute:02}{second:02}")
}

/// A path as a git argument: the one conversion every call needs, and the reason none of them takes a
/// `String` built with `format!`.
fn path(value: &Path) -> String {
    value.to_string_lossy().into_owned()
}

/// Run `git -C <repository> <args>` and answer git's words on failure.
fn git(repository: &Path, args: &[&str]) -> Result<(), BackupError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .map_err(|error| BackupError::Git(format!("git is not runnable: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Err(BackupError::Git(if stderr.is_empty() {
        stdout
    } else {
        stderr
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-04T00:00:00Z, the day the tests below are about.
    const DAY_2026_11_04: i64 = 1_793_750_400;

    #[test]
    fn a_known_instant_is_the_day_it_falls_in() {
        assert_eq!(day(DAY_2026_11_04), "2026-11-04");
        // The last second of that day is still that day, and the next second is the next one.
        assert_eq!(day(DAY_2026_11_04 + DAY - 1), "2026-11-04");
        assert_eq!(day(DAY_2026_11_04 + DAY), "2026-11-05");
        // The epoch itself, and a leap day, because the calendar arithmetic is the part that is easy to
        // get wrong and impossible to notice: a bundle named after the wrong day would be pruned early.
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(951_782_400), "2000-02-29");
    }

    #[test]
    fn only_this_projects_own_bundles_are_its_own() {
        let prefix = "holosmith-";
        for name in ["holosmith-2026-11-04.bundle", "holosmith-1970-01-01.bundle"] {
            assert!(is_bundle_of(name, prefix), "{name}");
        }
        for name in [
            // Another project whose name begins with this one's.
            "holosmith2-2026-11-04.bundle",
            // A half-written bundle, which is never a day's.
            "holosmith-2026-11-04.bundle.tmp",
            // A name that is not a day.
            "holosmith-yesterday.bundle",
            "holosmith-2026-11-4.bundle",
            "holosmith-2026_11_04.bundle",
            "holosmith-2026-11-04.bundle.bak",
            "other-2026-11-04.bundle",
        ] {
            assert!(!is_bundle_of(name, prefix), "{name}");
        }
    }

    /// A directory that removes itself, for the one test here that touches a filesystem. The crate's
    /// integration tests share `tests/support`'s `TempDir`; a unit test cannot reach it, and this is the
    /// only filesystem test in the module.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("holosmith-backup-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("the scratch directory is creatable");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_bundle_git_cannot_read_is_deleted_rather_than_kept() {
        // The story's fourth line, and the one branch of `take` a test cannot otherwise reach: git always
        // writes a bundle it can verify, so the only way to see the delete is to hand `keep` a file that
        // is not one. Deleting the `remove_file` there leaves the whole suite green, which is why this
        // exists.
        let scratch = Scratch::new("corrupt");
        let backups = Backups::new(&scratch.0);
        let repository = scratch.0.join("repository");
        std::fs::create_dir_all(&repository).expect("the repository directory is creatable");
        let bundle = backups.root.join("holosmith-2026-11-04.bundle");
        let partial = bundle.with_extension("bundle.tmp");
        std::fs::create_dir_all(&backups.root).expect("the backups directory is creatable");
        std::fs::write(&partial, b"not a bundle").expect("the junk file is writable");

        assert!(
            backups.keep(&repository, &partial, &bundle).is_err(),
            "a file that is not a bundle verified"
        );
        assert!(!partial.exists(), "the corrupt bundle was kept aside");
        assert!(
            !bundle.exists(),
            "a corrupt bundle took the name git trusts"
        );
    }

    #[test]
    fn the_directory_a_machine_keeps_is_below_its_data_root() {
        assert_eq!(
            Backups::new(Path::new("/var/lib/holosmith")).root,
            Path::new("/var/lib/holosmith/backups")
        );
    }
}

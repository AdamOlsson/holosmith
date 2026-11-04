//! The daily project backup: one git bundle per project per day, on the host (HS-065).
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
//!   operator would trust.
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

/// The directory under the data root that holds every project's bundles: `<data>/backups`.
pub const BACKUPS: &str = "backups";

/// How many days of bundles one project keeps: a week.
pub const KEEP: usize = 7;

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
}

impl fmt::Display for BackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(words) => write!(formatter, "git: {words}"),
            Self::Files(words) => write!(formatter, "{words}"),
        }
    }
}

/// The backups of one machine: the directory they live in, and the day they are named after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backups {
    /// `<data_root>/backups`.
    pub root: PathBuf,
}

impl Backups {
    /// The backups of the machine whose data root is `data_root`.
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
    /// first on a day, and not a failure.
    pub fn take(
        &self,
        repository: &Path,
        project: &str,
        now: i64,
    ) -> Result<Option<PathBuf>, BackupError> {
        let bundle = self.root.join(format!("{project}-{}.bundle", day(now)));
        if bundle.exists() {
            return Ok(None);
        }
        std::fs::create_dir_all(&self.root).map_err(|error| {
            BackupError::Files(format!("creating {}: {error}", self.root.display()))
        })?;

        // Written aside and renamed into place, so the name that exists is always a bundle that
        // verified: a half-written file under the final name would be a recovery point an operator
        // would trust and git would refuse.
        let partial = bundle.with_extension("bundle.tmp");
        let created = git(repository, &["bundle", "create", &path(&partial), "--all"]);
        if let Err(error) = created {
            let _ = std::fs::remove_file(&partial);
            return Err(error);
        }
        if let Err(error) = git(repository, &["bundle", "verify", &path(&partial)]) {
            // A corrupt bundle is deleted rather than kept (the story's fourth line): a file that cannot
            // be restored must not sit in the directory looking like one that can.
            let _ = std::fs::remove_file(&partial);
            return Err(error);
        }
        std::fs::rename(&partial, &bundle).map_err(|error| {
            let _ = std::fs::remove_file(&partial);
            BackupError::Files(format!("renaming {}: {error}", partial.display()))
        })?;
        self.prune(project)?;
        Ok(Some(bundle))
    }

    /// Delete all but the newest [`KEEP`] bundles of `project`, answering how many went.
    ///
    /// By name, because the name carries the day and a date sorts as text: no bundle is opened to decide
    /// which are oldest, which matters because the point of the prune is to be cheap on a machine that
    /// is already short of space.
    pub fn prune(&self, project: &str) -> Result<usize, BackupError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            // Nothing to prune: a machine whose backups directory has never been made has none.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
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
        Ok(excess)
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

    #[test]
    fn the_directory_a_machine_keeps_is_below_its_data_root() {
        assert_eq!(
            Backups::new(Path::new("/var/lib/holosmith")).root,
            Path::new("/var/lib/holosmith/backups")
        );
    }
}

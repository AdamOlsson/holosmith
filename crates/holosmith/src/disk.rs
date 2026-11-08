//! The free-space floor: one measurement, three readers (HS-062, spec item 14).
//!
//! Item 14's first sentence is "`min_free_gb` (default 20): session creation and resume fail with 507 below
//! it", and item 10 §1 puts the same floor in the boot's preflight, which `doctor --quick` prints as well.
//! Three readers of one number, so the number is measured **once** and this module is that once: [`measure`]
//! runs `df` and reads the answer through [`available_gb`], [`verdict`] is the comparison, and the only thing
//! a caller writes for itself is the sentence — because the boot's fix ("install coreutils", "there is no data
//! root yet") is not the create's ("free some space"). "The same measurement" is then a function call rather
//! than a convention two modules have to keep in step (ADR-0068).
//!
//! The measurement is `df -Pk` rather than `statvfs` for HS-024's reason: the crate denies `unsafe`, so the
//! syscall is not reachable without a `libc` dependency, and a POSIX `df` is on every machine item 10 §1
//! describes.
//!
//! Everything here is synchronous and std-only, like [`crate::changes`], [`crate::publish`] and
//! [`crate::worktree`]: the request path runs it on `spawn_blocking`, which is the rule every subprocess on a
//! request path follows.

use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::config::Role;
use crate::runtime::Backend;

/// The program the measurement runs, named once.
///
/// `preflight::Tools::default` starts its `df` from this, and a request path looks the same name up on the
/// same `PATH`: one spelling of the program, so the boot and a create cannot measure with two.
pub const DF: &str = "df";

/// How many kibibytes a gibibyte is: the divisor that turns `df`'s `Available` column into the whole number
/// of GB every sentence prints.
const KIB_PER_GIB: u64 = 1024 * 1024;

/// Why the free space could not be measured.
///
/// Four arms because the boot already answers four different sentences — and because each one carries exactly
/// the piece its sentence needs, so a caller composes words rather than re-deriving the failure. They are
/// **not** a "the disk is full" answer: a machine whose `df` cannot be run has not been measured at all, and
/// item 14's `.is` rule is that a check that cannot run must not report success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskError {
    /// There is no data root yet, so there is no filesystem to measure.
    ///
    /// This is the arm `doctor --quick` answers as `unknown`: `server deploy` runs it before step 2 has created
    /// the directory, and `df` on a missing path is an error — answering that as "the disk is full" would make
    /// a fresh host fail a check that has nothing to measure (ADR-0054).
    Missing,
    /// `df` ran and refused. Its own words are the fix's context, so they travel.
    Refused(String),
    /// `df` is not runnable at all — not installed, or not executable.
    Unrunnable(String),
    /// `df` answered something that is not a POSIX `df -Pk` answer.
    Unprintable(String),
}

impl fmt::Display for DiskError {
    /// The reason, for a log line. A caller that answers a client writes its own sentence: these words describe
    /// the machine, and item 14's refusal has to describe the request.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("there is no data root to measure"),
            Self::Refused(said) => write!(f, "`df` refused the data root ({said})"),
            Self::Unrunnable(error) => write!(f, "df is not runnable ({error})"),
            Self::Unprintable(text) => {
                write!(f, "`df` printed {text:?}, which is not a POSIX df answer")
            }
        }
    }
}

/// How the measured free space stands against the floor.
///
/// A named answer rather than a `bool` so that the two readers of it — the boot's finding and a request's
/// refusal — cannot disagree about which side of the comparison they are on, and so that `min_free_gb = 0`
/// reads as the rule it is: `available >= min` is always true against `0`, so `0` means "no floor" (ADR-0068).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// At or above `min_free_gb`: a session may start.
    Enough,
    /// Below `min_free_gb`: a new session must be refused.
    Short,
}

/// The comparison, in one place.
pub fn verdict(available_gb: u64, min_free_gb: u64) -> Verdict {
    if available_gb >= min_free_gb {
        Verdict::Enough
    } else {
        Verdict::Short
    }
}

/// The available-gigabytes number out of a POSIX `df -Pk` answer: the fourth field of the second line,
/// divided down to whole GB.
///
/// `-P` is what makes it one line per filesystem, so the columns are fixed and no header text is read; the
/// filesystem name is a path and cannot contain a space in the answer `df -P` prints for a plain path. The
/// division truncates, which is the conservative direction for a floor: 20.9 GB free reads as 20.
pub fn available_gb(df_output: &str) -> Option<u64> {
    let line = df_output.lines().nth(1)?;
    let available_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(available_kb / KIB_PER_GIB)
}

/// Measure `data_root`'s filesystem, in whole GB, with `df`.
///
/// Blocking: this forks a program. A request path calls it on a blocking task (every other subprocess on a
/// request path does), and `serve` calls it once, before the listener binds, where a synchronous call is what
/// a startup question wants.
pub fn measure(df: &Path, data_root: &Path) -> Result<u64, DiskError> {
    if !data_root.exists() {
        return Err(DiskError::Missing);
    }
    let output = match Command::new(df).arg("-Pk").arg(data_root).output() {
        Ok(output) if output.status.success() => output,
        // A `df` that ran and refused has said something about *this* filesystem — a data root under a
        // directory this user cannot traverse, say — and "install coreutils" would be advice about a program
        // that just answered.
        Ok(output) => {
            return Err(DiskError::Refused(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Err(error) => return Err(DiskError::Unrunnable(error.to_string())),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    available_gb(&text).ok_or_else(|| DiskError::Unprintable(text.to_string()))
}

/// Whether a run of `backend` and `role` is measured at all.
///
/// The floor guards a *container's* use of the filesystem: a session that provisions one writes build caches
/// and a worktree into gigabytes, while a stub session starts no container and needs no free space beyond its
/// own database. So the question is `preflight::check`'s own predicate — "only a Docker run is checked", and a
/// `hub` runs no sessions at all (spec item 10 §6's deploy skips the Docker checks) — written once so the boot
/// and the two request paths cannot answer it differently (ADR-0068).
pub fn guarded(backend: Backend, role: Role) -> bool {
    backend == Backend::Docker && role != Role::Hub
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                          /dev/mapper/root 1000000000 1 5242880 1% /\n";

    #[test]
    fn the_number_is_the_fourth_field_of_the_second_line_in_whole_gigabytes() {
        // 5 GiB of available kibibytes. The long device name and the long path are what push the columns, which
        // is why the field is counted rather than the text matched.
        assert_eq!(available_gb(ANSWER), Some(5));
        assert_eq!(
            available_gb(
                "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                 /dev/very/long/device-name 1000000000 1 1073741824 1% /a/deep/path\n"
            ),
            Some(1024)
        );
        // The truncation is the conservative direction: 20.99 GB is 20.
        assert_eq!(
            available_gb(
                "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                 /dev/sda1 1 1 22020096 1% /\n"
            ),
            Some(21)
        );
        assert_eq!(available_gb(""), None);
        assert_eq!(
            available_gb("Filesystem 1024-blocks Used Available Capacity Mounted on\n"),
            None
        );
        assert_eq!(available_gb("not a df answer at all\n"), None);
    }

    #[test]
    fn the_comparison_is_at_or_above_and_zero_is_no_floor() {
        assert_eq!(verdict(20, 20), Verdict::Enough);
        assert_eq!(verdict(19, 20), Verdict::Short);
        assert_eq!(verdict(0, 20), Verdict::Short);
        // `min_free_gb = 0` is legal and means "no floor" (ADR-0068): a story that only asked for a refusal
        // does not get a config rule.
        assert_eq!(verdict(0, 0), Verdict::Enough);
    }

    #[test]
    fn a_data_root_that_is_not_there_is_not_a_disk_problem() {
        // ADR-0054's arm, kept in the measured half rather than in a sentence: `doctor --quick` runs before
        // `server deploy` has created anything, and `serve` creates the directory before it asks.
        let missing = Path::new("/nonexistent-holosmith-data-root");
        assert_eq!(measure(Path::new(DF), missing), Err(DiskError::Missing));
    }

    #[test]
    fn only_a_container_run_is_guarded_and_a_hub_never_is() {
        assert!(guarded(Backend::Docker, Role::Server));
        assert!(guarded(Backend::Docker, Role::Both));
        assert!(!guarded(Backend::Stub, Role::Server));
        assert!(!guarded(Backend::Docker, Role::Hub));
    }

    /// The files allowed to name the measurement outside this one: the measurement itself, the boot's check, and
    /// the request-path refusal.
    ///
    /// `http/disk.rs` and not `http/sessions.rs` — the file that holds `prompt`, `abort` and `steer` beside the
    /// two starts must stay *checked*, or the guard would exempt exactly where a disk read on a turn's path would
    /// be added. That is why the refusal has a module of its own (PER-94's review).
    const THE_MEASURED_HALF: &str = "disk.rs";
    const THE_BOOT: &str = "preflight.rs";
    const THE_REQUEST_PATHS: &str = "http/disk.rs";

    /// Every file under `root` that names `disk::measure` outside the three allowed ones, as `<file>:<line>`.
    ///
    /// One function, so the rule and any planted test share the matcher. It is what makes item 14's "running
    /// turns are not interrupted; only new starts are refused" a fact about the tree rather than a claim: the
    /// floor has exactly the callers that *start* work, and a turn, a ladder, a sweeper, a provisioner — or a
    /// handler in `http/sessions.rs` — that grew a disk read would fail here. Both callers spell it
    /// `crate::disk::measure`, so the token is stable (and a hand-rolled `Command::new("df")` elsewhere is what
    /// the boot's and the request path's *sentences* are for: `tests/disk.rs` compares their numbers).
    fn offenders_under(root: &Path) -> Vec<String> {
        let mut offenders = Vec::new();
        for (file, text) in crate::sql::guard::sources_under(root) {
            if file == THE_MEASURED_HALF || file == THE_BOOT || file == THE_REQUEST_PATHS {
                continue;
            }
            for (line, text) in text.lines().enumerate() {
                if text.contains("disk::measure") {
                    offenders.push(format!("{file}:{}: disk::measure", line + 1));
                }
            }
        }
        offenders
    }

    #[test]
    fn nothing_but_a_start_reads_the_disk() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        // Non-vacuous: a walk that found nothing would pass the next assertion for the wrong reason, so this
        // one names the files the rule is about.
        let files: Vec<String> = crate::sql::guard::sources_under(&root)
            .into_iter()
            .map(|(file, _)| file)
            .collect();
        for expected in [THE_MEASURED_HALF, THE_BOOT, THE_REQUEST_PATHS] {
            assert!(
                files.iter().any(|file| file == expected),
                "the walk did not find src/{expected}"
            );
        }
        let offenders = offenders_under(&root);
        assert!(
            offenders.is_empty(),
            "something that does not start a session reads the disk: {offenders:?}"
        );
    }

    #[test]
    fn the_guard_fires_on_a_planted_read() {
        // The rule above passes over today's tree; this proves the same matcher would notice, and plants it in a
        // running turn — the file item 14's fourth line is about.
        let planted = crate::sql::guard::TempDir::new("disk-floor");
        planted.plant(
            "turn.rs",
            "if crate::disk::measure(Path::new(\"df\"), &config.data_root).is_err() {\n",
        );
        planted.plant("ladder.rs", "let available = 20;\n");
        let offenders = offenders_under(planted.path());
        assert_eq!(offenders.len(), 1, "{offenders:?}");
        assert!(offenders[0].starts_with("turn.rs:1: "), "{offenders:?}");
    }
}

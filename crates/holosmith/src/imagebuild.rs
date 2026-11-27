//! The images a session's first start has to build, lazily, under two locks, with a log an operator can read.
//!
//! A project's image is `hs-<project>:<digest>` — a content address over the recipe's bytes and the base image's tag
//! ([`crate::projectimage`]) — so "is it built?" is one existence check and nothing else. What has to *make* it is
//! this module: no step of a deploy could, because a project's recipe arrives after the project does, from a person,
//! in a git commit. So the container step builds what `docker run` needs, before it runs anything.
//!
//! Four things about that build are load-bearing, and each has its own paragraph below:
//!
//! - **The base comes first, and it is not the project's build.** A project with no recipe runs
//!   [`crate::images::base_image`], and a project with one builds `FROM` it, so the first session on a machine that
//!   never ran `server deploy` — or whose tags a `docker rmi`, a failed deploy or the gate's own image case removed —
//!   meets a missing base before it meets a missing project image. The base is the cheaper build and the one a recipe
//!   depends on, so ensuring it first is what makes a project's build a single attempt rather than one that fails
//!   inside `FROM`. Its build is `server deploy`'s step 3: the same [`crate::images::ensure`] over the same embedded
//!   bytes with the same [`crate::images::Expectation`], so a machine cannot end up with a base whose labels came from
//!   a different path than a deploy's.
//! - **Two locks, both files under the data root, and the machine's is the one that excludes.** The machine's lock is
//!   what makes two creates a second apart run one build, and what makes a project's build wait on the base's instead
//!   of racing it: it bounds this program to one build at a time, and every taker of an image's lock holds it already.
//!   The image's lock names the image under the data root, and it is what a second `holosmith image build` of that
//!   image would have to take — the half that becomes load-bearing the day the cap rises above one. Files rather than
//!   an in-memory map because `holosmith image build` is a second process and cannot see the map a session start uses
//!   (ADR-0037). [`Lock`] says how a holder is told from a pid that was recycled.
//! - **A bound, not a timeout to fail by.** [`crate::images::PROJECT_BUILD_TIMEOUT`] is an hour and
//!   [`crate::images::BUILD_TIMEOUT`] the base's; a build that reaches either is a sentence, never a wedged session.
//! - **The output lands in two places.** [`crate::images::failure`] is a bounded tail — the layer, the instruction and
//!   the child's last line — and the whole of it is `<data>/images/<repository>.log`, which the failure's sentence
//!   names. The build that writes a log truncates it, so a second build's failure is not read as the first's.
//!
//! **A build has the internet; the session it produces does not.** The daemon builds on its own default bridge, and
//! the container joins an `--internal` network ([`crate::container`]): that asymmetry is deliberate and is the first
//! thing an operator with a recipe that needs `apt-get` has to be told, which is why it is in the module an author
//! reads and in the ADR.
//!
//! **What is deliberately not here:** any cap on the bytes a build writes. Images live in the daemon's storage rather
//! than under the data root, and may be on another machine entirely when `docker_host` points elsewhere, so
//! `session_disk_soft_gb` cannot see them and this module does not pretend to — `docker system df` is the read.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::container::ContainerSettings;
use crate::docker::Docker;
use crate::images::{self, BuildError, Input, Kind, Log};
use crate::progress::Progress;

/// The directory under the data root that holds every build's log: `<data>/images`.
pub const DIRECTORY: &str = "images";

/// The subdirectory the lock files live in, so `<data>/images` lists logs and nothing else: `<data>/images/locks`.
pub const LOCKS: &str = "locks";

/// The word these lines are about, as [`Progress`] spells it.
///
/// A deploy's label is its report's step number (`3/6 images`); a session's build has no report — the row is the
/// report — so the word names the subject and nothing else.
pub const LABEL: &str = "image";

/// How long a build may wait for a lock another build holds.
///
/// Two bounded builds are the longest legitimate hold — a base and a project, each with its own bound — so a wait
/// longer than both is a lock nobody will release, and the answer is a sentence rather than a `starting` row that
/// waits for the rest of the process's life. It is deliberately generous: the failure it replaces is a duplicate
/// build, and the failure it risks is a session that gives up while a real build is still running.
const LOCK_WAIT: Duration = Duration::from_secs(
    images::BUILD_TIMEOUT.as_secs() + images::PROJECT_BUILD_TIMEOUT.as_secs() + 60,
);

/// What a session queued behind another build says on its row, for as long as it waits.
///
/// [`LOCK_WAIT`] is over an hour and a half — two bounded builds and a minute — so the wait is long enough that a row
/// saying nothing is a session that looks wedged, which is the row ADR-0124's D5 exists to prevent.
const WAITING: &str = "waiting for another image build to finish";

/// How long a lock file that names no holder yet is given before it is read as abandoned.
///
/// The creator makes the file and writes its identity a moment later, so a file that is empty or half-written is
/// usually a live lock. Five seconds is longer than a scheduler pause and shorter than anything an operator notices.
const LOCK_GRACE: Duration = Duration::from_secs(5);

/// Whether an image that is already there is rebuilt.
///
/// A session's start wants [`Self::Missing`]: that is what makes a second session free and what the gate measures by
/// the image's `Created`. [`Self::Forced`] is `image build --force`'s, and it is the only way to rebuild a tag the
/// digest says is current — a recipe whose `COPY`'d file moved is stale with the same tag, and nothing reads that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Currency {
    Missing,
    Forced,
}

/// Where one image's whole build log lives: `<data>/images/<repository>.log`, the repository being the tag before
/// its `:`.
///
/// One file per image and not per tag: a tag's digest moves on every recipe edit, so `hs-foo:<digest>.log` would be a
/// file per edit and `hs-foo.log` is the log of the last build of `hs-foo`, whatever digest it was under. A project
/// genuinely named `base` gives `hs-base.log`, which is not the base image's `holosmith-base.log`.
pub fn log_path(data_root: &Path, image: &str) -> PathBuf {
    data_root
        .join(DIRECTORY)
        .join(format!("{}.log", repository(image)))
}

/// The machine's lock: one build at a time on this machine, whether it is the base's or a project's.
pub fn machine_lock(data_root: &Path) -> PathBuf {
    data_root.join(DIRECTORY).join(LOCKS).join("machine.lock")
}

/// One image's lock: `hs-foo.lock`, naming the image under `<data>/images/locks`.
///
/// The machine's ([`machine_lock`]) is the lock that excludes: a taker of this one holds it already, so no two takers
/// of this one can overlap today. This is what a second `holosmith image build` of the *same* image would have to
/// take, which is the day it becomes the excluding lock rather than the name of one.
pub fn image_lock(data_root: &Path, image: &str) -> PathBuf {
    data_root
        .join(DIRECTORY)
        .join(LOCKS)
        .join(format!("{}.lock", repository(image)))
}

/// `image` without its tag: `hs-foo:1a2b…` is the repository `hs-foo`.
fn repository(image: &str) -> &str {
    image
        .split_once(':')
        .map_or(image, |(repository, _)| repository)
}

/// Ensure every image `project`'s next container needs, building what is missing: the base first, the project's own
/// tag after.
///
/// The answer is the verb's sentence — `built hs-foo:…`, or why nothing was built — and a session's start ignores it
/// because the row already says what the step is doing. `Err` is a build that failed or did not finish, with the
/// whole log's path in the sentence.
///
/// `project_root` is `<projects root>/<project>`, the directory the recipe is read from and the context Docker is
/// handed. The two builds are one call because they are one start: the machine's lock is taken for the whole of it, so
/// no other build runs between a base this call built and a project image that builds `FROM` it.
pub fn ensure(
    docker: &Docker,
    settings: &ContainerSettings,
    project: &str,
    project_root: &Path,
    currency: Currency,
    progress: &mut dyn Progress,
) -> Result<String, BuildError> {
    // The recipe is read before anything is locked or built: a project whose recipe cannot become an image is
    // refused without spending a base build first, and that answer depends on no lock.
    let resolved = crate::projectimage::resolve(project_root, project, &settings.image)
        .map_err(|problem| BuildError::from(problem.sentence()))?;
    let _machine = Lock::acquire(
        machine_lock(&settings.data_root),
        LOCK_WAIT,
        Some(&mut *progress),
    )?;
    let mut built = Vec::new();
    // The base only when this machine's sessions actually run it: a `base_image` naming another image — this
    // repository's `agent-image`, or a hand-built one — is not a recipe Holosmith has read, so a build here would be
    // this program guessing what an image it has never seen should be. `server deploy`'s step 3 builds those.
    if settings.image == images::base_image() {
        // `Currency::Missing` and not the caller's: `--force` names the subject it was given, and the base is this
        // build's dependency rather than its subject — a project's `--force` that also rebuilt the base would be
        // minutes spent on something the caller did not ask for.
        built.extend(base(docker, settings, Currency::Missing, progress)?);
    }
    let Some(recipe) = &resolved.build else {
        let nothing = format!(
            "{project} has no {}, so its sessions run {}: nothing to build",
            crate::projectimage::RECIPE,
            resolved.image
        );
        return Ok(if built.is_empty() {
            nothing
        } else {
            format!("{}; {nothing}", built_sentence(&built))
        });
    };
    built.extend(project_image(
        docker,
        settings,
        &resolved.image,
        recipe,
        currency,
        progress,
    )?);
    Ok(if built.is_empty() {
        format!(
            "{} is already there, so nothing was built: --force rebuilds it",
            resolved.image
        )
    } else {
        built_sentence(&built)
    })
}

/// Ensure the base image alone: what `image build --base` pre-warms, and the first half of [`ensure`].
///
/// The tag is [`crate::images::base_image`] and not the config's `base_image`: a pre-warm is for the image Holosmith
/// builds and a deploy installs, and a config that names another image is a fact the verb's sentence carries rather
/// than a reason to build something else.
pub fn ensure_base(
    docker: &Docker,
    settings: &ContainerSettings,
    currency: Currency,
    progress: &mut dyn Progress,
) -> Result<String, BuildError> {
    let _machine = Lock::acquire(
        machine_lock(&settings.data_root),
        LOCK_WAIT,
        Some(&mut *progress),
    )?;
    let built = base(docker, settings, currency, progress)?;
    let mut sentence = if built.is_empty() {
        format!(
            "{} is already there, so nothing was built: --force rebuilds it",
            images::base_image()
        )
    } else {
        built_sentence(&built)
    };
    if settings.image != images::base_image() {
        sentence.push_str(&format!(
            "; the config's `base_image` names {}, so a session whose project has no recipe runs that image and not \
             this one",
            settings.image
        ));
    }
    Ok(sentence)
}

/// The base image, built from the recipe embedded in this binary when the tag is not there.
///
/// The read is the tag's existence and nothing else: the base carries this binary's labels, so a deploy's skip rule
/// compares four of them, but a session only has to *run* it — and a tag that is there was built by something, which
/// is the same answer `projectimage::resolve` gives a project's own image. A base whose labels are another release's
/// is `server deploy`'s question, and it is asked by a deploy.
fn base(
    docker: &Docker,
    settings: &ContainerSettings,
    currency: Currency,
    progress: &mut dyn Progress,
) -> Result<Vec<String>, BuildError> {
    let tag = images::base_image();
    if currency == Currency::Missing && docker.image_exists(&tag) {
        return Ok(Vec::new());
    }
    let log = log_path(&settings.data_root, &tag);
    // The digest is a label's and not a question here, but `ensure` will not build without one: an image it built
    // carries the four values, and a base built here is the same image a deploy would have built.
    let digest = images::own_digest()?;
    let binary = std::env::current_exe().map_err(|error| {
        format!(
            "the base image was not built: this process's own path could not be read ({error}), and the build's \
             context is an archive holding the Dockerfile and this program"
        )
    })?;
    let release = crate::version::VersionInfo::current();
    let pending = vec![(
        Kind::Base,
        format!("{tag} is not on this machine, so this session's container has no base to run"),
    )];
    images::ensure(
        docker,
        &binary,
        &digest,
        &release,
        &pending,
        LABEL,
        Some(&log),
        progress,
    )?;
    Ok(vec![tag])
}

/// One project's image, built from its own recipe when the tag is not there.
///
/// The context is the project root and the recipe is `docker/Dockerfile` inside it — named with `-f`, because Docker
/// reads `Dockerfile` from the context's own root — which is what makes a `COPY Cargo.lock …` work and what Docker
/// filters by the project's own `.dockerignore` (ADR-0123). No `--build-arg` is passed: a project's image carries no
/// Holosmith label, and an argument a recipe does not declare is a BuildKit warning over nothing.
fn project_image(
    docker: &Docker,
    settings: &ContainerSettings,
    tag: &str,
    recipe: &crate::projectimage::Build,
    currency: Currency,
    progress: &mut dyn Progress,
) -> Result<Vec<String>, BuildError> {
    let _image = Lock::acquire(
        image_lock(&settings.data_root, tag),
        LOCK_WAIT,
        Some(&mut *progress),
    )?;
    // The check is inside the lock, and that is the whole point of the lock: two creates a second apart both see an
    // absent tag, and only the one that wins the file builds it.
    if currency == Currency::Missing && docker.image_exists(tag) {
        return Ok(Vec::new());
    }
    let log = log_path(&settings.data_root, tag);
    progress.note(LABEL, &format!("building {tag}"));
    let arguments = images::build_head(tag);
    let built = images::Images::build(
        docker,
        tag,
        Input::Directory {
            recipe: &recipe.recipe,
            context: &recipe.context,
        },
        &arguments,
        Some(&log),
    )?;
    let success = built.success;
    progress.note(
        LABEL,
        &format!("{} {tag}", if success { "built" } else { "did not build" }),
    );
    match built {
        Log { success: true, .. } => Ok(vec![tag.to_string()]),
        Log { text, .. } => Err(BuildError {
            sentence: format!(
                "{}{}",
                images::failure(tag, &text),
                images::log_clause(Some(&log))
            ),
            fetch_did_not_answer: images::fetch_did_not_answer(&text),
        }),
    }
}

/// The verb's answer for the images this call built: `built a, b`.
fn built_sentence(built: &[String]) -> String {
    format!("built {}", built.join(", "))
}

/// One build's lock: an exclusive file under `<data>/images/locks`, naming the process that holds it.
///
/// There is no `flock` here — `unsafe_code` is denied and this crate's dependencies are the image's, so a crate for it
/// is a rebuild of the image that vendors them — so the exclusion is `O_EXCL` on the file's creation and the identity
/// is what the file says: `<pid> <starttime>`, the pair the gate's own cleanup compares, which is what tells a live
/// holder from a number the kernel has since handed to something else.
///
/// **A lock that outlives its holder is "this project can never start a session again"**, which is worse than the
/// duplicate build the lock exists to prevent. So a holder is *detected* rather than trusted: a file whose pid is not
/// running, or is running with another start time, is stale, and whoever finds it removes it and tries again. A file
/// with no identity yet is the one case that needs a wait rather than a decision — its creator has made it and has not
/// written the pid — so an unreadable lock is given [`LOCK_GRACE`] from its modification time first.
///
/// The removal on the way out is guarded by the same identity: a lock this process took and lost to a breaker must not
/// take the next holder's file away.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
    holder: String,
}

impl Lock {
    /// Take `path`, waiting for whoever holds it until `wait` runs out.
    ///
    /// `progress` is the row's sink, and it is told **once**, when the first attempt finds the file held: a session
    /// that is queued behind another build sits `starting` with no sentence otherwise, and that is the row D5 exists
    /// to prevent — the wait is bounded by [`LOCK_WAIT`], which is over an hour and a half. The note is a wait's and
    /// not a build's, so an uncontended acquire says nothing at all.
    fn acquire(
        path: PathBuf,
        wait: Duration,
        progress: Option<&mut dyn Progress>,
    ) -> Result<Self, BuildError> {
        // The locks' own directory, made on the way: a machine that has never built an image has no
        // `<data>/images` at all, and a lock that cannot be created is a session that cannot start.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                BuildError::from(format!(
                    "{} could not be made ({error}): it is where one build at a time is recorded, so no image can \
                     be built until the data root is writable",
                    parent.display()
                ))
            })?;
        }
        let holder = identity();
        let deadline = Instant::now() + wait;
        let mut progress = progress;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write as _;
                    // A write that fails leaves a file with no identity, which [`stale`] reads as abandoned after
                    // the grace: the lock is held either way, and the failure is not worth a sentence.
                    let _ = writeln!(file, "{holder}");
                    return Ok(Self { path, holder });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(BuildError::from(format!(
                        "{} could not be made ({error}): one build at a time is what keeps two sessions from \
                         running the same `docker build`, and a lock that cannot be written cannot say whether one \
                         is running",
                        path.display()
                    )));
                }
            }
            // Somebody holds it, so this is a wait and the row may say so. Taken (moved out) so a loop that waits
            // out a stale lock and then a live one says it once.
            if let Some(sink) = progress.take() {
                sink.note(LABEL, WAITING);
            }
            if stale(&path) {
                match std::fs::remove_file(&path) {
                    // Somebody else broke the same lock, which is the outcome this wanted.
                    Err(error) if error.kind() != ErrorKind::NotFound => {
                        return Err(BuildError::from(format!(
                            "{} names a process that is gone and could not be removed ({error}): it is the lock one \
                             build at a time is made of, so no image can be built until it is deleted",
                            path.display()
                        )));
                    }
                    _ => continue,
                }
            }
            if Instant::now() >= deadline {
                return Err(BuildError::from(format!(
                    "another image build held {} for {}s, so this one did not start: the lock names the process \
                     holding it, and a build that long is a build that is stuck rather than one still working",
                    path.display(),
                    wait.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|text| text.trim() == self.holder) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// This process's identity as a lock writes it: `<pid> <starttime>`.
fn identity() -> String {
    let pid = std::process::id();
    format!("{pid} {}", start_time(pid).unwrap_or(0))
}

/// A process's start time, in clock ticks since boot, from `/proc/<pid>/stat`.
///
/// The field is the kernel's answer to "which process is this number", and it is what makes a recycled pid a
/// different process rather than the holder. `None` for a pid that is not running and for a `/proc` that cannot be
/// read, which are the same answer here: nothing says the holder is alive.
fn start_time(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The second field is the command, parenthesised and able to hold spaces and `)`, so the fields after it are
    // counted from the **last** `)`: the first token there is field 3 (the state) and the start time is field 22.
    let (_, rest) = text.rsplit_once(')')?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Whether the lock at `path` was abandoned by its holder.
///
/// Three arms, and each is a different reason to believe nobody is coming back: the pid is not running, the pid is
/// running as another process (a recycled number), or the file has not said who it is and is older than
/// [`LOCK_GRACE`]. A file that cannot be read at all is not broken here — a lock this process cannot read is one it
/// cannot say anything about, and the wait's deadline is the answer to that.
fn stale(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut fields = text.split_whitespace();
    let (Some(pid), Some(ticks)) = (fields.next(), fields.next()) else {
        return older_than(path, LOCK_GRACE);
    };
    let Ok(pid) = pid.parse::<u32>() else {
        return older_than(path, LOCK_GRACE);
    };
    match start_time(pid) {
        None => true,
        // A pid that is alive under a start time this file does not name is another process; a start time that is
        // not a number is a file this tree did not write, and waiting out the deadline for it would be worse.
        Some(started) => ticks.parse::<u64>() != Ok(started),
    }
}

/// Whether `path` was last modified longer ago than `age`.
///
/// A file whose modification time cannot be read is *not* old: nothing measured it, and "abandoned" is a claim that
/// needs a measurement.
fn older_than(path: &Path, age: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|elapsed| elapsed > age)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch data root named for the test, made fresh and removed by the test that made it.
    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "holosmith-imagebuild-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the scratch directory is creatable");
        path
    }

    /// A sink that keeps what it is told, so a test can read a row's sentence rather than only a lock's file.
    #[derive(Default)]
    struct Noted(Vec<String>);

    impl Progress for Noted {
        fn note(&mut self, _label: &str, phrase: &str) {
            self.0.push(phrase.to_string());
        }
    }

    #[test]
    fn a_log_is_one_file_per_repository_and_two_images_cannot_share_one() {
        let data = Path::new("/data");
        assert_eq!(
            log_path(data, "hs-foo:1a2b3c"),
            Path::new("/data/images/hs-foo.log"),
            "a project's log is not named for its repository"
        );
        // The digest moves on every recipe edit, and the log does not: it is the last build of `hs-foo`, whatever
        // digest it was under.
        assert_eq!(
            log_path(data, "hs-foo:ffff"),
            Path::new("/data/images/hs-foo.log")
        );
        assert_eq!(
            log_path(data, &images::base_image()),
            Path::new(&format!(
                "/data/images/{}.log",
                repository(&images::base_image())
            ))
        );
        // A project genuinely named `base` is `hs-base`, which is not the base image's repository.
        assert_ne!(
            log_path(data, "hs-base:d"),
            log_path(data, &images::base_image())
        );
        // An image with no tag at all is its own repository rather than an empty name.
        assert_eq!(
            log_path(data, "hs-foo"),
            Path::new("/data/images/hs-foo.log")
        );
    }

    #[test]
    fn the_two_locks_are_files_under_the_data_root_and_a_project_is_not_the_machine() {
        let data = Path::new("/data");
        assert_eq!(
            machine_lock(data),
            Path::new("/data/images/locks/machine.lock")
        );
        assert_eq!(
            image_lock(data, "hs-foo:1a2b3c"),
            Path::new("/data/images/locks/hs-foo.lock")
        );
        // The machine's lock is one file for the machine, and a project's is one per repository: a build of one
        // project must not wait on another project's, and every build must wait on the machine's.
        assert_ne!(machine_lock(data), image_lock(data, "hs-foo:1a2b3c"));
        assert_ne!(
            image_lock(data, "hs-foo:1a2b3c"),
            image_lock(data, "hs-bar:1a2b3c")
        );
    }

    #[test]
    fn a_lock_is_taken_once_and_a_second_taker_sees_it_held() {
        let root = scratch("taken");
        // Under a directory that is not there: a machine that has never built an image has no `<data>/images`, and a
        // lock that could not be created would be a session that could not start.
        let path = root.join(LOCKS).join("machine.lock");
        // An uncontended lock says nothing on the row: the sentence is a wait's, and a build that never waited would
        // be telling an operator about a queue it was not in.
        let mut immediate = Noted::default();
        let held = Lock::acquire(
            path.clone(),
            Duration::from_millis(200),
            Some(&mut immediate),
        )
        .expect("an unheld lock is taken");
        assert!(
            immediate.0.is_empty(),
            "an unheld lock put a sentence on the row: {:?}",
            immediate.0
        );
        // The holder is this process, so the file names a pid that is running with its own start time: not stale.
        assert!(!stale(&path), "this process's own lock read as abandoned");
        // A taker that has to wait is the row D5 is about: it sat `starting` with nothing to say, for as long as
        // `LOCK_WAIT` — which is the wait this note is bounded by.
        let mut waited = Noted::default();
        let refused = Lock::acquire(path.clone(), Duration::from_millis(200), Some(&mut waited));
        assert!(
            refused.is_err(),
            "two takers of one lock both got it: {:?}",
            refused.map(|lock| lock.path.clone())
        );
        assert_eq!(
            waited.0,
            vec![WAITING],
            "a taker that waited said nothing on the row, or said it more than once"
        );
        drop(held);
        assert!(
            !path.exists(),
            "the lock was not removed when its holder let go"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lock_whose_process_is_gone_is_broken_rather_than_waited_for() {
        let root = scratch("stale");
        let path = root.join("hs-foo.lock");
        // A pid nothing is running: the kernel's maximum plus one, which `fork` cannot hand out.
        let gone = std::process::id() + 1_000_000;
        std::fs::write(&path, format!("{gone} 12345\n")).expect("the lock is writable");
        assert!(stale(&path), "a lock naming a dead pid read as held");
        let taken = Lock::acquire(path.clone(), Duration::from_millis(500), None)
            .expect("a stale lock is broken and taken");
        assert_eq!(
            std::fs::read_to_string(&path)
                .expect("the new holder wrote its identity")
                .trim(),
            identity(),
            "the lock a second taker broke does not name the second taker"
        );
        drop(taken);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lock_naming_this_pid_with_another_start_time_is_a_recycled_number() {
        let root = scratch("recycled");
        let path = root.join("hs-bar.lock");
        // The pid is this process's and the start time is not, which is what a number the kernel handed to a later
        // process looks like: the file is that process's, not this one's.
        std::fs::write(&path, format!("{} 1\n", std::process::id())).expect("the lock is writable");
        assert!(stale(&path), "a recycled pid read as a live holder");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lock_that_has_not_said_who_it_is_yet_is_given_a_grace_and_not_broken_at_once() {
        let root = scratch("writing");
        let path = root.join("hs-baz.lock");
        // The creator has made the file and has not written the pid: a second taker that broke it at once would
        // start the duplicate build this lock exists to prevent.
        std::fs::write(&path, "").expect("the lock is writable");
        assert!(!stale(&path), "a lock being written was read as abandoned");
        // Older than the grace with nothing in it is a writer that died mid-line.
        let old = std::time::SystemTime::now() - (LOCK_GRACE + Duration::from_secs(60));
        set_modified(&path, old);
        assert!(
            stale(&path),
            "an empty lock older than the grace read as held"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lock_this_process_lost_is_not_removed_on_its_way_out() {
        let root = scratch("lost");
        let path = root.join("hs-lost.lock");
        let held = Lock::acquire(path.clone(), Duration::from_millis(200), None)
            .expect("an unheld lock is taken");
        // A breaker read it as stale and a second build took it: the file now names somebody else, and this
        // process's `drop` must leave it alone or the second build's lock would go with it.
        std::fs::write(&path, "999999999 1\n").expect("the lock is writable");
        drop(held);
        assert!(
            path.exists(),
            "a holder removed a lock another build had taken"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Set `path`'s modification time, which is the one thing [`older_than`] reads.
    fn set_modified(path: &Path, at: std::time::SystemTime) {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("the lock is openable");
        file.set_modified(at).expect("the lock's mtime is settable");
    }
}

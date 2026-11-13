//! The release tree: `releases/<tag>/` and the `current` symlink systemd runs (spec item 10).
//!
//! Item 10's first paragraph is the whole of this module: "releases live in `releases/<tag>/` with a `current`
//! symlink that systemd runs". Three facts follow, and each is a decision this file owns:
//!
//! * **Where.** The release tree is `<root>/releases`, inside the one directory a machine has (ADR-0086): the
//!   service user owns it, so `upgrade` and `rollback` write plain files and need no privilege for the tree.
//! * **What a release is.** Two files, `holosmith` and `holosmith.sha256`, which are exactly the two
//!   `holosmith build --out` writes ([`crate::builder::BINARY`] and [`crate::builder::DIGEST`]). The directory's
//!   name is the tag the binary itself reports (`version`'s `git_tag`), so a release directory cannot be named
//!   after a different build than the one inside it — and one tag holding two binaries is refused rather than
//!   overwritten ([`crate::builder::Placement`]'s rule, one level out).
//! * **How it is switched.** `current` is a **relative** symlink (`releases/<tag>`), so the tree stays relocatable,
//!   created beside the link and `rename`d onto it — the atomic replace, never an edit in place and never a
//!   remove-then-create (acceptance bullet for, and the reason a promotion's switch is one syscall).
//!
//! # What the layout means
//!
//! Three facts follow from "releases side by side with one live pointer", and they are all here because they are all
//! statements about the tree:
//!
//! * **Which ones are kept.** [`Release::prune`] keeps `keep_releases` directories — always the live release and the
//!   one behind it (the rollback target), then the newest of the rest — by [`history`](Release::history), and never
//!   touches a directory the tree does not recognise as a release.
//! * **Which one is running.** [`running_tag`] answers, from the *path of the running file*, which release directory it
//!   came out of; `holosmith version` reports it as `release`, and `std::env::current_exe` resolves the `current`
//!   symlink, so a binary systemd started through the link reports the release it is really in.
//! * **Whether the unit can start.** `doctor`'s release finding asks [`Release::live`] and [`unit_executable`] together,
//!   because a `current` the unit executes through and that resolves to nothing is a machine systemd cannot start.
//!
//! What needed is also here: the tree's creation (a first promotion seeds the previous release out of the binary
//! the unit runs), the switch, and the answer "what does the unit execute" ([`Release::live`]), which `deploy`'s step
//! 5 reads so that a later `server deploy` neither undoes a promotion nor rewrites a correct unit.
//!
//! Nothing here starts a process or reads a database: a release tree is directories, two files and one symlink.

use std::path::{Path, PathBuf};

use crate::deploy::Root;

/// The directory the releases live in, under the root: `<root>/releases`.
pub const RELEASES: &str = "releases";

/// The symlink systemd executes through.
pub const CURRENT: &str = "current";

/// A release's binary, and its digest: [`crate::builder`]'s two names, because a release directory is what
/// `holosmith build --out releases/<tag>` produces.
pub const BINARY: &str = crate::builder::BINARY;
pub const DIGEST: &str = crate::builder::DIGEST;

/// The file a release's binary is written under before it is renamed into place.
const PENDING: &str = crate::builder::PENDING;

/// What [`Release::prune`] did.
///
/// Both lists are in [`history`](Release::history)'s order (newest first), so a caller's line reads the same way the
/// tree does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pruned {
    /// Every release the tree still holds: the protected two and the newest of the rest.
    pub kept: Vec<String>,
    /// The release directories this call removed.
    pub removed: Vec<String>,
}

/// What [`Release::write`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written {
    /// The binary was not there, and is now.
    Fresh,
    /// The directory already held these exact bytes: nothing was written.
    Current,
}

/// One machine's release tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// `<root>/releases`.
    root: PathBuf,
}

impl Release {
    /// The tree of the [`Root`] a run was given: the root itself, with `releases/` and `current` under it.
    pub fn of(root: &Root) -> Self {
        Self::at(root.path())
    }

    /// The tree at an explicit path, which is what the tests drive.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `<root>/releases`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/releases`.
    pub fn releases(&self) -> PathBuf {
        self.root.join(RELEASES)
    }

    /// The symlink's own path: `<root>/current`.
    pub fn current(&self) -> PathBuf {
        self.root.join(CURRENT)
    }

    /// `releases/<tag>`: one release's directory.
    pub fn dir_of(&self, tag: &str) -> PathBuf {
        self.releases().join(tag)
    }

    /// The binary a release's directory holds.
    pub fn binary(&self, tag: &str) -> PathBuf {
        self.dir_of(tag).join(BINARY)
    }

    /// The link's target, exactly as it was written (relative, unless an operator made it absolute).
    ///
    /// Verbatim rather than resolved, because the rollback writes this value back: a link an operator pointed at
    /// an absolute path is theirs, and replacing it with a relative one would be a change nobody asked for.
    pub fn link(&self) -> Option<PathBuf> {
        std::fs::read_link(self.current()).ok()
    }

    /// The tag a link target names, when it is one of this tree's own.
    ///
    /// The only shape accepted is [`RELEASES`]`/<tag>` (or the same through an absolute path): a link to anywhere
    /// else is an operator's, and a promotion refuses rather than guessing what "the previous release" means — see
    /// [`crate::promote`].
    pub fn tag_of(&self, link: &Path) -> Option<String> {
        let rest = if link.is_absolute() {
            link.strip_prefix(&self.root).ok()?.to_path_buf()
        } else {
            link.to_path_buf()
        };
        let rest = rest.strip_prefix(RELEASES).ok()?;
        let tag = rest.to_str()?;
        (!tag.is_empty() && tag_problem(tag).is_none()).then(|| tag.to_string())
    }

    /// Every release this tree holds, **newest first**.
    ///
    /// This is the tree's one *order*, and it is the order the releases were **written onto this machine**: a
    /// directory's modification time, which the kernel sets when [`write`](Self::write) creates it (and refreshes when
    /// a re-promotion rewrites the same bytes' digest). A directory whose name is not one path segment, or that holds
    /// no `holosmith` binary, is not a release and is skipped — a release a rollback cannot start is not a place to go
    /// back to.
    ///
    /// It is deliberately **not** a name order. A release directory is named after `git describe`'s answer
    /// (`f2b3886-dirty`, `0.1.0-3-gabc1234`), so sorting by name would put a hash above a version tag and a rollback
    /// would walk into whichever build happened to sort next. Two entries written in the same instant are ordered by
    /// tag, so the order is total and two runs of this function agree about it.
    ///
    /// the pruning keeps the newest N of this list; that is why the order lives here rather than in a verb.
    pub fn history(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.releases()) else {
            return Vec::new();
        };
        let mut found: Vec<(std::time::SystemTime, String)> = Vec::new();
        for entry in entries.flatten() {
            let Some(tag) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if tag_problem(&tag).is_some() {
                continue;
            }
            let path = entry.path();
            if !path.join(BINARY).is_file() {
                continue;
            }
            let Ok(modified) = std::fs::metadata(&path).and_then(|meta| meta.modified()) else {
                continue;
            };
            found.push((modified, tag));
        }
        found.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        found.into_iter().map(|(_, tag)| tag).collect()
    }

    /// The release one step back from `tag`: the newest one **strictly older** than it in [`history`](Self::history).
    ///
    /// `None` when `tag` is the oldest release the tree holds, or is not in the tree at all, or the tree holds one
    /// release — each is a tree a rollback has nowhere to go back to in, and each is a different sentence at the
    /// caller.
    pub fn older_than(&self, tag: &str) -> Option<String> {
        let history = self.history();
        let at = history.iter().position(|known| known == tag)?;
        history.get(at + 1).cloned()
    }

    /// The tags a prune may never remove: the one `current` names, and the one [`older_than`](Self::older_than) it.
    ///
    /// Two tags, and both are a rollback's: a tree that cannot go back has lost the property the layout exists for.
    /// Empty when `current` is missing or names something which is not one of this tree's releases — there is nothing
    /// to protect then, and the history's own order is all a prune has to go on.
    fn protected(&self) -> Vec<String> {
        let Some(tag) = self.link().and_then(|link| self.tag_of(&link)) else {
            return Vec::new();
        };
        let mut tags = vec![tag.clone()];
        tags.extend(self.older_than(&tag));
        tags
    }

    /// Keep the tree at `keep` releases, newest first, and never the live one or the rollback target.
    ///
    /// The rule, in one paragraph: the **protected** two (see [`protected`](Self::protected)) are reserved first and
    /// count towards `keep`; the remaining room is filled with the newest entries of [`history`](Self::history); every
    /// other release directory is removed. So a tree of exactly `keep` releases is left alone, a larger tree is pruned
    /// to `keep`, and a count smaller than the protected set (which the config refuses) still keeps both — this
    /// function's own floor, so a caller that passed `0` cannot delete the machine's way back.
    ///
    /// **Which directories may be deleted: only `history()`'s.** A directory whose name is not one path segment
    /// (`tag_problem`) or that holds no `holosmith` is not a release — it is never counted and never touched, so an
    /// operator's `releases/notes/` survives. A directory that *is* a release is a release however it got there: a
    /// hand-seeded `releases/old` is in the order and may be pruned like any other, which is what "pruned to a
    /// configured count" means for a tree somebody copied into.
    ///
    /// The error carries what was removed before it stopped: a prune deletes in order and stops at the first directory
    /// it cannot delete, because continuing past a failure would report a tree it did not measure.
    pub fn prune(&self, keep: u32) -> Result<Pruned, String> {
        let history = self.history();
        let protected = self.protected();
        let reserved = history.iter().filter(|tag| protected.contains(tag)).count();
        let mut room = (keep as usize).saturating_sub(reserved);
        let mut kept: Vec<String> = Vec::new();
        for tag in &history {
            if protected.contains(tag) {
                kept.push(tag.clone());
            } else if room > 0 {
                room -= 1;
                kept.push(tag.clone());
            }
        }
        let mut removed: Vec<String> = Vec::new();
        for tag in &history {
            if kept.contains(tag) {
                continue;
            }
            let dir = self.dir_of(tag);
            std::fs::remove_dir_all(&dir).map_err(|error| {
                let removed = if removed.is_empty() {
                    "nothing was removed".to_string()
                } else {
                    format!("releases/{} was removed", removed.join(", releases/"))
                };
                format!(
                    "releases/{tag} could not be removed ({error}): a prune keeps the newest `keep_releases` and \
                     never the live release or the one behind it, and it stops at the first directory it cannot \
                     delete — {removed} and whatever else the tree holds is untouched"
                )
            })?;
            removed.push(tag.clone());
        }
        Ok(Pruned { kept, removed })
    }

    /// The binary systemd executes: `<root>/current/holosmith`, when the tree is live.
    ///
    /// The **path through the link**, not the directory it resolves to, because that is the whole point of the
    /// layout: systemd re-resolves `current` at every (re)start, so a switch is what a restart picks up. `None` is
    /// a tree with no `current`, or one whose link names no binary — a machine that has not promoted yet.
    pub fn live(&self) -> Option<PathBuf> {
        let link = self.link()?;
        let tag = self.tag_of(&link)?;
        self.binary(&tag)
            .is_file()
            .then(|| self.current().join(BINARY))
    }

    /// Write one release's two files, or refuse a directory that already holds a different binary for this tag.
    ///
    /// The bytes are written aside (`builder::PENDING`, the name `holosmith build` also uses) and renamed into
    /// place, so the file that exists under `holosmith` is always a whole binary. A directory holding the *same*
    /// bytes is [`Written::Current`] and writes only the digest when that is missing — the idempotent re-run
    /// [`builder::place`] is.
    pub fn write(&self, tag: &str, bytes: &[u8], digest: &str) -> Result<Written, String> {
        if let Some(problem) = tag_problem(tag) {
            return Err(problem);
        }
        let dir = self.dir_of(tag);
        std::fs::create_dir_all(&dir).map_err(|error| {
            format!(
                "the release directory {} could not be created ({error})",
                dir.display()
            )
        })?;
        let artifact = dir.join(BINARY);
        if artifact.is_file() {
            let existing = crate::images::digest(&artifact)?;
            if existing == digest {
                write_digest(&dir, digest)?;
                return Ok(Written::Current);
            }
            return Err(format!(
                "{} already holds a different binary for {tag} (sha256 {existing}, and this candidate is \
                 {digest}): nothing was replaced, because one release directory holding two binaries is the \
                 ambiguity a rollback cannot resolve — build the candidate under its own tag, or move that \
                 directory away",
                artifact.display()
            ));
        }
        let pending = dir.join(PENDING);
        write_binary(&pending, bytes)?;
        std::fs::rename(&pending, &artifact).map_err(|error| {
            let _ = std::fs::remove_file(&pending);
            format!(
                "the candidate could not be moved into place at {} ({error})",
                artifact.display()
            )
        })?;
        write_digest(&dir, digest)?;
        Ok(Written::Fresh)
    }

    /// Point `current` at `releases/<tag>`, atomically.
    ///
    /// A symlink is created beside the link and `rename`d onto it: `rename(2)` replaces the old link in one step,
    /// so a reader (systemd, a shell, the next promotion) sees either the old target or the new one and never a
    /// moment with no `current` at all. The target is written **relative** so the tree can be moved or renamed as
    /// a whole.
    pub fn switch(&self, tag: &str) -> Result<PathBuf, String> {
        if let Some(problem) = tag_problem(tag) {
            return Err(problem);
        }
        let target = Path::new(RELEASES).join(tag);
        self.restore(&target)?;
        Ok(target)
    }

    /// Point `current` at `target` **verbatim**, atomically.
    ///
    /// [`switch`](Self::switch) is this function with a relative `releases/<tag>`; a rollback is this function with
    /// the link target it read before the switch, whatever that was. Verbatim because the link was an operator's or a
    /// previous promotion's, and putting back a *different* spelling of it would be a change nobody asked for.
    pub fn restore(&self, target: &Path) -> Result<(), String> {
        std::fs::create_dir_all(self.releases()).map_err(|error| {
            format!(
                "{} could not be created ({error}): a promotion writes the release it is promoting there",
                self.releases().display()
            )
        })?;
        let staged = self
            .root
            .join(format!("{CURRENT}.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&staged);
        std::os::unix::fs::symlink(target, &staged).map_err(|error| {
            format!(
                "the symlink {} could not be created ({error})",
                staged.display()
            )
        })?;
        if let Err(error) = std::fs::rename(&staged, self.current()) {
            let _ = std::fs::remove_file(&staged);
            return Err(format!(
                "{} could not be replaced by a symlink to {} ({error}): the switch is a rename, so a failure here \
                 leaves the previous release live",
                self.current().display(),
                target.display()
            ));
        }
        Ok(())
    }
}

/// The release tag the **running file** is in: `<tree>/releases/<tag>/holosmith` answers `Some(tag)`.
///
/// The fact is about the *path the process was started from*, not about a build: a hand-seeded
/// `releases/one/holosmith` (a copy of any binary, which is what the gate's fixtures are) reports `one`, while the
/// same file's `git_tag` is the tag it was built from. That difference is the point — a release directory's name is
/// what a promotion chose, and it is the name `current`, `releases/` and a rollback all speak in.
///
/// On Linux `std::env::current_exe()` reads `/proc/self/exe`, which the kernel resolves, so a binary systemd started
/// through `current/holosmith` reports the release directory it is really in rather than the link. The rule itself is
/// pure and takes a path, so its arms are a unit test's: the file is named `holosmith` ([`BINARY`]), its parent passes
/// [`tag_problem`], and its grandparent is named [`RELEASES`]. Anything else — a binary in `dist/`, a file beside a
/// release — is `None`, which the CLI prints as `-`.
///
/// Nothing is read from the disk: naming a release is a path's shape, and a `version` that stat'ed its own tree would
/// answer a question the operator did not ask.
pub fn running_tag(exe: &Path) -> Option<String> {
    if exe.file_name()?.to_str()? != BINARY {
        return None;
    }
    let dir = exe.parent()?;
    let tag = dir.file_name()?.to_str()?;
    if tag_problem(tag).is_some() {
        return None;
    }
    (dir.parent()?.file_name()?.to_str()? == RELEASES).then(|| tag.to_string())
}

/// Why `tag` cannot name a release directory, or `None` when it can.
///
/// One path segment and nothing else: a tag is `git describe`'s answer and may legitimately carry `v`, `.`, `-` and
/// a short hash, but a `/` in it would put the release somewhere else in the tree, and `.`/`..` would name a
/// directory that is not a release at all. A leading `-` is refused too, because the tag is printed and passed to
/// nothing today — and a value that looks like a flag is a habit worth not starting.
pub fn tag_problem(tag: &str) -> Option<String> {
    if tag.is_empty() {
        return Some(
            "the candidate reports an empty git tag, so its release directory would have no name: build it from a \
             tree `git describe` can answer for, or tag the commit the candidate was built from (item 10's \
             `releases/<tag>/`)"
                .to_string(),
        );
    }
    if tag == "." || tag == ".." || tag.starts_with('-') {
        return Some(format!(
            "the candidate reports the git tag {tag:?}, which cannot name a release directory: a release is \
             `releases/<tag>/` and the tag has to be one path segment that is not `-`, `.` or `..`"
        ));
    }
    let clean = tag.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '+')
    });
    if !clean || tag.contains('/') {
        return Some(format!(
            "the candidate reports the git tag {tag:?}, and a release directory's name has to be one path \
             segment of letters, digits, `.`, `-`, `_` and `+`: a tag with a `/` in it would put the release \
             somewhere else in the tree (item 10's `releases/<tag>/`)"
        ));
    }
    None
}

/// The program a unit file's `ExecStart=` names.
///
/// One reader with two callers that must agree about one fact — what is systemd actually running — and the unit is
/// where that fact lives: [`crate::deploy::unit_text`] writes `ExecStart=<exe> serve`, so a promotion that has no
/// `current` yet learns the previous release from the binary named here.
///
/// The first word after the key, which is systemd's own rule (the rest are arguments). A leading `-`, `@`, `+` or
/// `!` is systemd's "ignore failure"/"argv[0]"/"privileged"/"credentials" marker and is stripped, because a unit an
/// operator edited by hand may carry one and the program is still the program. `ExecStartPre=` is a different key
/// and does not match: the comparison is on the whole `ExecStart=` prefix.
pub fn unit_executable(text: &str) -> Option<PathBuf> {
    let value = text
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("ExecStart="))?;
    let program = value.trim_start_matches(['-', '@', '+', '!']).trim();
    let first = program.split_whitespace().next()?;
    (!first.is_empty()).then(|| PathBuf::from(first))
}

/// Write a release's digest file in `sha256sum`'s own format, so an operator's `sha256sum -c` reads it.
fn write_digest(dir: &Path, digest: &str) -> Result<(), String> {
    let path = dir.join(DIGEST);
    std::fs::write(&path, format!("{digest}  {BINARY}\n")).map_err(|error| {
        format!(
            "the release's binary is in place but its digest could not be written to {} ({error})",
            path.display()
        )
    })
}

/// Write `bytes` to `path` as an executable file, created fresh.
fn write_binary(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let _ = std::fs::remove_file(path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(path)
        .map_err(|error| {
            format!(
                "{} could not be created ({error}): a release's binary is written beside its name and renamed \
                 into place",
                path.display()
            )
        })?;
    file.write_all(bytes)
        .map_err(|error| format!("{} could not be written ({error})", path.display()))?;
    // The mode is set at `open`, but the umask may have cleared bits a second run then finds missing: an
    // executable a service can start is the one property a release's binary cannot lose.
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .map_err(|error| format!("{} could not be made executable ({error})", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_is_one_path_segment() {
        for tag in [
            "v0.1.0",
            "0.1.0-3-gabc1234",
            "v0.1.0-dirty",
            "release_1",
            "1.2.3+build",
        ] {
            assert_eq!(tag_problem(tag), None, "{tag}");
        }
        for tag in ["", ".", "..", "-rf", "a/b", "a b", "a\tb", "näme"] {
            assert!(tag_problem(tag).is_some(), "{tag:?} was accepted");
        }
    }

    #[test]
    fn the_exec_start_is_read_and_the_pre_line_is_not() {
        let unit = crate::deploy::unit_text(
            Path::new("/usr/local/bin/holosmith"),
            crate::config::Role::Server,
            &crate::deploy::Root::resolve(Some(PathBuf::from("/var/lib/holosmith"))),
        );
        assert_eq!(
            unit_executable(&unit),
            Some(PathBuf::from("/usr/local/bin/holosmith")),
            "the unit `unit_text` writes must be the unit this reader reads"
        );
        // The same rendering for a tree under its own root: the same ExecStart, that root's paths.
        let rooted = crate::deploy::unit_text(
            Path::new("/tree/bin/holosmith"),
            crate::config::Role::Server,
            &crate::deploy::Root::resolve(Some(PathBuf::from("/tree"))),
        );
        assert_eq!(
            unit_executable(&rooted),
            Some(PathBuf::from("/tree/bin/holosmith"))
        );
        assert_eq!(
            unit_executable("[Service]\nExecStart=/opt/holosmith/current/holosmith serve\n"),
            Some(PathBuf::from("/opt/holosmith/current/holosmith"))
        );
        // systemd's four prefixes, and a unit with no ExecStart at all.
        assert_eq!(
            unit_executable("ExecStart=+/usr/bin/x serve"),
            Some(PathBuf::from("/usr/bin/x"))
        );
        assert_eq!(unit_executable("[Unit]\nDescription=x\n"), None);
        assert_eq!(unit_executable("ExecStart=\n"), None);
    }

    /// A tree under a temporary directory, removed on drop: the tests below measure real directories' mtimes.
    struct Tree(PathBuf);

    impl Tree {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("holosmith-release-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join(RELEASES)).expect("a temporary release tree");
            Self(path)
        }

        fn release(&self) -> Release {
            Release::at(&self.0)
        }

        /// Write one release with an explicit mtime, so the order is the test's and not the clock's.
        fn write(&self, tag: &str, seconds: u64) {
            let release = self.release();
            let digest = "0".repeat(64);
            release
                .write(tag, b"not a real binary", &digest)
                .expect("the release is written");
            set_mtime(&release.dir_of(tag), seconds);
        }

        /// Point `current` at `tag`, relatively, the way a promotion's switch does.
        fn link(&self, tag: &str) {
            let release = self.release();
            let _ = std::fs::remove_file(release.current());
            std::os::unix::fs::symlink(Path::new(RELEASES).join(tag), release.current())
                .expect("the link is created");
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Set a directory's own mtime, which is the fact [`Release::history`] orders by.
    fn set_mtime(path: &Path, seconds: u64) {
        let directory = std::fs::File::open(path).expect("the directory is open");
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        directory.set_modified(when).expect("the mtime is set");
    }

    #[test]
    fn the_history_is_the_order_the_releases_were_written_in() {
        let tree = Tree::new("history");
        tree.write("v0.1.0", 1_000);
        tree.write("f2b3886-dirty", 3_000);
        tree.write("0.1.0-3-gabc1234", 2_000);
        // A name order is not the order: sorted by name this would be `0.1.0-3-gabc1234`, `f2b3886-dirty`,
        // `v0.1.0`, and a rollback would walk into a hash.
        assert_eq!(
            tree.release().history(),
            vec![
                "f2b3886-dirty".to_string(),
                "0.1.0-3-gabc1234".to_string(),
                "v0.1.0".to_string()
            ]
        );
        // One step back, from each place: this is what the verb's default target is.
        assert_eq!(
            tree.release().older_than("f2b3886-dirty"),
            Some("0.1.0-3-gabc1234".to_string())
        );
        assert_eq!(
            tree.release().older_than("0.1.0-3-gabc1234"),
            Some("v0.1.0".to_string())
        );
        assert_eq!(
            tree.release().older_than("v0.1.0"),
            None,
            "the oldest has nothing behind it"
        );
        assert_eq!(tree.release().older_than("not-a-release"), None);
    }

    #[test]
    fn a_directory_that_is_not_a_release_is_not_in_the_history() {
        let tree = Tree::new("skipped");
        tree.write("real", 1_000);
        // A directory whose name cannot be a tag, one that holds no binary, and one that is a plain file.
        std::fs::create_dir_all(tree.release().releases().join("a b")).expect("a directory");
        std::fs::create_dir_all(tree.release().releases().join("empty")).expect("a directory");
        std::fs::write(tree.release().releases().join("stray.txt"), b"").expect("a file");
        assert_eq!(tree.release().history(), vec!["real".to_string()]);
        assert_eq!(tree.release().older_than("real"), None);
    }

    #[test]
    fn two_releases_written_in_one_instant_still_have_an_order() {
        // A tie in mtime is broken by tag, so the list is total and a rollback's default target is not a coin toss.
        let tree = Tree::new("ties");
        tree.write("beta", 5_000);
        tree.write("alpha", 5_000);
        assert_eq!(
            tree.release().history(),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn a_link_target_that_is_not_one_of_this_trees_releases_has_no_tag() {
        let release = Release::at("/opt/holosmith");
        assert_eq!(
            release.tag_of(Path::new("releases/v0.1.0")),
            Some("v0.1.0".to_string())
        );
        assert_eq!(
            release.tag_of(Path::new("/opt/holosmith/releases/v0.1.0")),
            Some("v0.1.0".to_string())
        );
        for other in [
            Path::new("/usr/local/bin/holosmith"),
            Path::new("releases"),
            Path::new("releases/"),
            Path::new("somewhere/v0.1.0"),
            Path::new("/opt/elsewhere/releases/v0.1.0"),
        ] {
            assert_eq!(release.tag_of(other), None, "{}", other.display());
        }
    }

    #[test]
    fn the_running_file_names_the_release_directory_it_is_in() {
        // The shape a machine's release has: `<tree>/releases/<tag>/holosmith`.
        assert_eq!(
            running_tag(Path::new("/opt/holosmith/releases/v0.1.0/holosmith")),
            Some("v0.1.0".to_string())
        );
        assert_eq!(
            running_tag(Path::new(
                "target/install/opt/holosmith/releases/f2b3886-dirty/holosmith"
            )),
            Some("f2b3886-dirty".to_string()),
            "the rule is about the path's shape, so an install tree's release is a release"
        );
        for other in [
            // The development build, and the artifact `build --out` writes: not in a release directory.
            Path::new("target/x86_64-unknown-linux-musl/debug/holosmith"),
            Path::new("dist/x86_64-unknown-linux-musl/holosmith"),
            // A release directory's digest file, a file in the wrong tree, and a tag no directory may be named.
            Path::new("/opt/holosmith/releases/v0.1.0/holosmith.sha256"),
            Path::new("/opt/holosmith/v0.1.0/holosmith"),
            Path::new("/opt/holosmith/releases/a b/holosmith"),
            Path::new("holosmith"),
            Path::new("releases/v0.1.0"),
        ] {
            assert_eq!(running_tag(other), None, "{}", other.display());
        }
    }

    #[test]
    fn a_prune_keeps_the_count_and_never_the_live_release_or_the_rollback_target() {
        let tree = Tree::new("prune");
        for (tag, seconds) in [
            ("one", 1_000),
            ("two", 2_000),
            ("three", 3_000),
            ("four", 4_000),
        ] {
            tree.write(tag, seconds);
        }
        // A directory the tree does not recognise — no binary in it — and one whose name cannot be a tag.
        std::fs::create_dir_all(tree.release().releases().join("notes")).expect("a directory");
        std::fs::create_dir_all(tree.release().releases().join("a b")).expect("a directory");
        tree.link("four");

        // Four releases, keep 2: the live one and the rollback target are the two newest, so the two oldest go.
        let pruned = tree.release().prune(2).expect("the tree is prunable");
        assert_eq!(pruned.kept, vec!["four".to_string(), "three".to_string()]);
        assert_eq!(pruned.removed, vec!["two".to_string(), "one".to_string()]);
        assert_eq!(tree.release().history(), pruned.kept);
        // What it did not recognise is still there: a prune deletes only what `history` calls a release.
        assert!(tree.release().releases().join("notes").is_dir());
        assert!(tree.release().releases().join("a b").is_dir());
        // A second prune with nothing to do removes nothing — the promotion that runs twice cannot fail on its own
        // tree.
        assert_eq!(
            tree.release().prune(2).expect("a second prune is fine"),
            Pruned {
                kept: vec!["four".to_string(), "three".to_string()],
                removed: Vec::new(),
            }
        );
        // Raising the count prunes nothing either.
        assert!(
            tree.release()
                .prune(9)
                .expect("a large count is fine")
                .removed
                .is_empty()
        );
    }

    #[test]
    fn a_prune_reserves_the_live_release_even_when_it_is_not_the_newest() {
        // An operator's switch, or a hand-seeded tree: `current` on the oldest release. "The newest N" alone would
        // delete the release the machine is running, which is exactly what the acceptance forbids.
        let tree = Tree::new("prune-oldest");
        for (tag, seconds) in [("one", 1_000), ("two", 2_000), ("three", 3_000)] {
            tree.write(tag, seconds);
        }
        tree.link("one");
        let pruned = tree.release().prune(2).expect("the tree is prunable");
        assert_eq!(pruned.kept, vec!["three".to_string(), "one".to_string()]);
        assert_eq!(pruned.removed, vec!["two".to_string()]);
        assert_eq!(
            tree.release().link().as_deref(),
            Some(Path::new("releases/one"))
        );
        assert!(tree.release().binary("one").is_file());
    }

    #[test]
    fn a_prune_keeps_both_protected_releases_when_the_count_cannot_hold_them() {
        // The config refuses a count below 2, so this is the function's own floor: a caller that passed 0 must not be
        // able to delete the machine's way back.
        let tree = Tree::new("prune-floor");
        for (tag, seconds) in [("one", 1_000), ("two", 2_000), ("three", 3_000)] {
            tree.write(tag, seconds);
        }
        tree.link("three");
        let pruned = tree.release().prune(0).expect("the tree is prunable");
        assert_eq!(pruned.kept, vec!["three".to_string(), "two".to_string()]);
        assert_eq!(pruned.removed, vec!["one".to_string()]);
    }
}

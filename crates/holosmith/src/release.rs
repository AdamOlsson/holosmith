//! The release tree: `releases/<tag>/` and the `current` symlink systemd runs (HS-078, spec item 10).
//!
//! Item 10's first paragraph is the whole of this module: "releases live in `releases/<tag>/` with a `current`
//! symlink that systemd runs". Three facts follow, and each is a decision this file owns:
//!
//! * **Where.** [`ROOT`] is `/opt/holosmith` — the FHS's home for an add-on application, a sibling of the
//!   `/etc/holosmith`, `/var/lib/holosmith` and `/run/holosmith` the deploy already writes — and
//!   [`crate::deploy::Target`] prefixes it, so an install tree (`--root`) carries its own release tree and an
//!   unprivileged run can drive the whole thing.
//! * **What a release is.** Two files, `holosmith` and `holosmith.sha256`, which are exactly the two
//!   `holosmith build --out` writes ([`crate::builder::BINARY`] and [`crate::builder::DIGEST`]). The directory's
//!   name is the tag the binary itself reports (`version`'s `git_tag`), so a release directory cannot be named
//!   after a different build than the one inside it — and one tag holding two binaries is refused rather than
//!   overwritten ([`crate::builder::Placement`]'s rule, one level out).
//! * **How it is switched.** `current` is a **relative** symlink (`releases/<tag>`), so the tree stays relocatable,
//!   created beside the link and `rename`d onto it — the atomic replace, never an edit in place and never a
//!   remove-then-create (acceptance bullet for HS-080, and the reason a promotion's switch is one syscall).
//!
//! # What this module does not own
//!
//! HS-080 owns what the layout *means* from here: pruning old releases to a configured count, `holosmith version`
//! reporting the tag it was run from, and `doctor` reporting the resolved release path and the link's integrity. What
//! HS-078 needed, and what is here, is the tree's creation (a first promotion seeds the previous release out of the
//! binary the unit runs), the switch, and the answer "what does the unit execute" ([`Release::live`]), which
//! `deploy`'s step 5 reads so that a later `server deploy` neither undoes a promotion nor rewrites a correct unit.
//!
//! Nothing here starts a process or reads a database: a release tree is directories, two files and one symlink.

use std::path::{Path, PathBuf};

use crate::deploy::Target;

/// The release tree's home, prefixed by [`crate::deploy::Target`] for an install tree.
pub const ROOT: &str = "/opt/holosmith";

/// The directory the releases live in, under [`ROOT`].
pub const RELEASES: &str = "releases";

/// The symlink systemd executes through.
pub const CURRENT: &str = "current";

/// A release's binary, and its digest: [`crate::builder`]'s two names, because a release directory is what
/// `holosmith build --out releases/<tag>` produces.
pub const BINARY: &str = crate::builder::BINARY;
pub const DIGEST: &str = crate::builder::DIGEST;

/// The file a release's binary is written under before it is renamed into place.
const PENDING: &str = crate::builder::PENDING;

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
    /// `<install>/opt/holosmith`.
    root: PathBuf,
}

impl Release {
    /// The tree of the install [`Target`] names.
    pub fn of(target: &Target) -> Self {
        Self {
            root: target.opt_dir(),
        }
    }

    /// The tree at an explicit path, which is what the tests drive.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `<install>/opt/holosmith`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<install>/opt/holosmith/releases`.
    pub fn releases(&self) -> PathBuf {
        self.root.join(RELEASES)
    }

    /// The symlink's own path: `<install>/opt/holosmith/current`.
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

    /// The binary systemd executes: `<install>/opt/holosmith/current/holosmith`, when the tree is live.
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
            true,
            crate::config::Role::Server,
        );
        assert_eq!(
            unit_executable(&unit),
            Some(PathBuf::from("/usr/local/bin/holosmith")),
            "the unit `unit_text` writes must be the unit this reader reads"
        );
        // A `--root` unit has no `User=` lines and the same ExecStart.
        let rooted = crate::deploy::unit_text(
            Path::new("/tree/bin/holosmith"),
            false,
            crate::config::Role::Server,
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
}

//! The account this process runs as: the uid a root invocation is refused on, and the home every default path is
//! derived from (ADR-0088, ADR-0090).
//!
//! The config and the state are the operator's own files: `~/.config/holosmith.toml` and
//! `~/.local/share/holosmith`. `~` is therefore a fact the binary has to answer, and the answer is the
//! **password entry of the uid the process runs as** — never `HOME`, and never a compile-time guess.
//!
//! `HOME` is not a fact about the account: a shell may export anything, an `ssh` may forward one, and `sudo`
//! sets it from its own policy. The password database has no such freedom: it describes the uid the process is
//! running as, so "which home" and "which uid" are one fact, and the uid is the fact that decides what the
//! process may read anyway. The run that would have made the difference visible — `sudo holosmith …`, whose `~`
//! is root's — never reaches a default path at all: uid 0 is refused before anything is dispatched
//! ([`root_refusal`]). `XDG_CONFIG_HOME`/`XDG_DATA_HOME` are refused for `HOME`'s reason: an operator who keeps
//! their data elsewhere says so with `HOLOSMITH_ROOT`, `--root` or `--config`.
//!
//! There is no `getuid` to call: this crate denies `unsafe` and carries no `libc`. The uid is therefore a
//! **read** — `/proc/self`'s owner is this process's own effective uid — and the passwd entry is parsed from
//! `/etc/passwd`. One module, so which account this is, and every default path derived from it, are one answer.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;

/// The one path whose owner is this process's own effective uid.
const SELF: &str = "/proc/self";

/// The password database: field 3 is the uid and field 6 the home.
const PASSWD: &str = "/etc/passwd";

/// The home a uid with no password entry gets, so every default is still an absolute path.
///
/// It is `/` because a uid the database does not describe — a container started with `--user <uid>`, a chroot
/// with no `passwd` — has no home to derive one from. The defaults then name `/.config/holosmith.toml` and
/// `/.local/share/holosmith`: paths no unprivileged process can write, so the first write fails loudly rather
/// than landing somewhere the operator did not name. A machine in that position says where its files are with
/// `--config`, `--root` or `HOLOSMITH_ROOT`.
pub const NO_HOME: &str = "/";

/// The uid a root invocation runs as: the one uid this program refuses, whatever was typed (ADR-0090).
pub const ROOT_UID: u32 = 0;

/// The effective uid of this process, or `None` when `/proc/self` cannot be read.
pub fn euid() -> Option<u32> {
    fs::metadata(SELF).ok().map(|metadata| metadata.uid())
}

/// The one sentence a root invocation gets, or `None` when `uid` may run this program (ADR-0090).
///
/// **Why refuse at all.** A root run "works" and then breaks the machine: the `0600` secrets and the database end
/// up owned by root, `doctor`'s permission check fails from then on, `deploy`'s directory actions report a change
/// they never make, and git calls the repository a stranger's. Every one of those is a fact about **which uid
/// wrote the file**, so the only place to stop it is before a file is opened — which is why `main` asks this
/// before it parses a command line.
///
/// **Why the sentence names the command.** What an operator who typed `sudo ./holosmith status` needs is the line
/// to type instead, so `command` — everything after the program name, verb, subcommand and flags included — is
/// carried through unchanged, and an empty one leaves the sentence ending at the program name rather than at a
/// dangling space.
///
/// `uid` is a parameter and not a read, so both answers are drivable by a test. It is [`euid`]'s — `/proc/self`'s
/// owner — because a crate that denies `unsafe` and carries no `libc` has no `getuid` to call; a uid it cannot
/// read is never a refusal, because an unread identity is not root.
pub fn root_refusal(uid: u32, command: &str) -> Option<String> {
    (uid == ROOT_UID).then(|| {
        let program = if command.is_empty() {
            String::from("holosmith")
        } else {
            format!("holosmith {command}")
        };
        format!("this runs as you, drop the sudo: {program}")
    })
}

/// The home [`euid`]'s password entry names, or [`NO_HOME`] when the database cannot name one — no entry for the
/// uid, or a `/proc/self` that cannot be read.
pub fn home() -> PathBuf {
    euid()
        .and_then(home_of)
        .unwrap_or_else(|| PathBuf::from(NO_HOME))
}

/// One uid's home, read from `/etc/passwd`: `None` when the file cannot be read or has no entry for `uid`.
pub fn home_of(uid: u32) -> Option<PathBuf> {
    let text = fs::read_to_string(PASSWD).ok()?;
    passwd_home(&text, uid)
}

/// The home field of the entry for `uid` in `passwd` text, or `None` when no line names it.
///
/// A line is `name:password:uid:gid:gecos:home:shell`, so the uid is field 3 and the home field 6. Blank lines
/// and `#` comments are skipped, and so is a line with too few fields to have a home — a malformed line is not
/// an entry, and the next one may be. The **first** match wins, which is what `getpwuid` answers on a database
/// with a duplicated uid. An empty home field counts as no home: a relative default path would be resolved
/// against a working directory this module knows nothing about.
pub fn passwd_home(passwd: &str, uid: u32) -> Option<PathBuf> {
    passwd
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find_map(|line| {
            let mut fields = line.split(':');
            let _name = fields.next()?;
            let _password = fields.next()?;
            let found = fields.next()?;
            let _gid = fields.next()?;
            let _gecos = fields.next()?;
            let home = fields.next()?;
            (found.trim().parse::<u32>() == Ok(uid)).then(|| PathBuf::from(home))
        })
        .filter(|home| !home.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A password database with the shapes an entry can have: two real accounts, a comment, a blank line, a
    /// malformed line, and two entries for one uid.
    const DATABASE: &str = "\
# the machine's accounts\n\
root:x:0:0:root:/root:/bin/bash\n\
\n\
svc:x:997:997::/var/lib/svc:/usr/sbin/nologin\n\
malformed:x:998\n\
twice:x:1000:1000::/home/first:/bin/bash\n\
twice-again:x:1000:1000::/home/second:/bin/bash\n\
me:x:1001:1001:Me,,,:/home/me:/bin/bash\n\
nohome:x:1002:1002:::/bin/sh\n";

    #[test]
    fn the_home_is_field_six_of_the_entry_for_the_uid() {
        assert_eq!(passwd_home(DATABASE, 0), Some(PathBuf::from("/root")));
        assert_eq!(passwd_home(DATABASE, 1001), Some(PathBuf::from("/home/me")));
        assert_eq!(
            passwd_home(DATABASE, 997),
            Some(PathBuf::from("/var/lib/svc")),
            "an entry with an empty gecos still has a home"
        );
    }

    #[test]
    fn the_first_entry_for_a_uid_is_the_one_that_answers() {
        assert_eq!(
            passwd_home(DATABASE, 1000),
            Some(PathBuf::from("/home/first"))
        );
    }

    #[test]
    fn a_uid_the_database_does_not_name_has_no_home() {
        assert_eq!(passwd_home(DATABASE, 4242), None);
        assert_eq!(passwd_home("", 0), None);
    }

    #[test]
    fn a_malformed_line_is_not_an_entry() {
        assert_eq!(passwd_home(DATABASE, 998), None);
    }

    #[test]
    fn an_empty_home_field_is_no_home() {
        assert_eq!(passwd_home(DATABASE, 1002), None);
    }

    #[test]
    fn a_uid_must_match_the_whole_field() {
        // `1000` is a prefix of `10001`: a parser that compared strings would answer the wrong account's home.
        let passwd = "a:x:10001:10001::/home/other:/bin/sh\nb:x:1000:1000::/home/me:/bin/sh\n";
        assert_eq!(passwd_home(passwd, 1000), Some(PathBuf::from("/home/me")));
        assert_eq!(passwd_home(passwd, 100), None);
    }

    #[test]
    fn root_is_refused_and_a_normal_uid_is_not() {
        assert_eq!(
            root_refusal(ROOT_UID, "status"),
            Some("this runs as you, drop the sudo: holosmith status".to_string())
        );
        assert_eq!(root_refusal(1000, "status"), None);
    }

    #[test]
    fn the_refusal_names_the_command_as_typed_and_a_bare_run_names_none() {
        assert_eq!(
            root_refusal(ROOT_UID, "server deploy --root /x"),
            Some("this runs as you, drop the sudo: holosmith server deploy --root /x".to_string())
        );
        // The sentence ends at the program name: a trailing space would be a command with nothing in it.
        assert_eq!(
            root_refusal(ROOT_UID, ""),
            Some("this runs as you, drop the sudo: holosmith".to_string())
        );
    }

    #[test]
    fn this_process_has_a_uid_and_the_home_that_uid_owns() {
        let uid = euid().expect("Linux has /proc/self, so the effective uid is readable");
        // The read agrees with the file it is a read of: `home_of` is the same fact as `passwd_home` over the
        // machine's own database.
        let text = fs::read_to_string(PASSWD).expect("this test needs the password database");
        assert_eq!(home_of(uid), passwd_home(&text, uid));
        // And the composed answer is that home, or the documented fallback — never a relative path.
        let home = home();
        assert!(
            home.is_absolute(),
            "{} is not an absolute path",
            home.display()
        );
        if home_of(uid).is_none() {
            assert_eq!(home, Path::new(NO_HOME));
        }
    }
}

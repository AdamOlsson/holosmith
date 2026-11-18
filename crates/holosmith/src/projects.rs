//! What a project's name is, and the name a repository address gives it.
//!
//! A project is a repository directly inside the projects root (the glossary's **Project**), and the name a
//! session is created against is the directory's own name. Two rules about that name live here so that the
//! create path and the command line cannot spell them twice:
//!
//! - **A name is one path segment** ([`name_is_valid`]): 1 to [`MAX_NAME_CHARS`] characters of letters,
//!   digits, `.`, `_` and `-`, and neither `.` nor `..`. That is what keeps `<projects_root>/<name>` inside
//!   the projects root, so it is the same rule whether the name arrived in a create's body or beside a URL.
//! - **A name can be derived from the address it was cloned from** ([`name_from_url`]): the URL's last path
//!   segment, less a trailing `/` and a trailing `.git`. `holosmith project add` derives rather than asks,
//!   because the name is what the new-session form takes and a reader who has to invent it can invent one the
//!   create refuses (ADR-0096).
//!
//! Nothing here reads a repository, runs git or writes a file: whether what a name points at may be mounted is
//! [`crate::worktree::refuse`]'s question, and the clone itself is `commands::project`'s.

/// `NAME_MAX`'s 255: builds `<projects_root>/<name>` from this value.
pub const MAX_NAME_CHARS: usize = 255;

/// Whether `name` may address a project: one path segment of letters, digits, `.`, `_` and `-`, 1 to
/// [`MAX_NAME_CHARS`] characters, and not `.` or `..`.
///
/// One spelling of the rule. The create validates a request's `project` with this
/// (`validate_project` in `http/sessions.rs`, which answers `bad_project`), and `project add` validates the
/// name it derived from a URL with it.
pub fn name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && name != "."
        && name != ".."
        && name.bytes().all(is_name_byte)
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

/// The project name `url` gives: its last path segment, less a trailing `/` and a trailing `.git`, checked
/// against [`name_is_valid`].
///
/// The `Err` is the sentence the operator is told, and it names both what was found and what a name may be,
/// because a URL and a deriver disagreeing is not something a bare "refused" explains: `https://host/you/..`
/// and `https://host/` both reach here, and neither has a name to offer. A name is never invented — a
/// made-up one would not be the directory the reader cloned, and would not be the name the next `project
/// add` of the same URL derives.
pub fn name_from_url(url: &str) -> Result<String, String> {
    // A trailing `/` is how a copied address often ends, and `git clone` accepts it; it is not a segment.
    let url = url.trim_end_matches('/');
    let segment = url.rsplit('/').next().unwrap_or_default();
    let name = segment.strip_suffix(".git").unwrap_or(segment);
    if name_is_valid(name) {
        return Ok(name.to_string());
    }
    Err(format!(
        "`{url}` names no project: its last path segment, less a trailing `/` and `.git`, is {segment:?}, \
         and a project name is 1 to {MAX_NAME_CHARS} characters of letters, digits, `.`, `_` and `-`, and \
         not `.` or `..`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_names_the_last_segment_less_a_trailing_slash_and_dot_git() {
        for (url, expected) in [
            ("https://github.com/you/project.git", "project"),
            ("https://github.com/you/project", "project"),
            ("https://github.com/you/project/", "project"),
            ("https://github.com/you/project///", "project"),
            ("git@github.com:you/project.git", "project"),
            ("file:///srv/git/my.repo_2-x", "my.repo_2-x"),
            ("/srv/git/plain-path", "plain-path"),
        ] {
            assert_eq!(name_from_url(url).as_deref(), Ok(expected), "{url}");
        }
    }

    /// A URL with no usable segment is refused with a sentence rather than answered with a name nobody typed.
    #[test]
    fn a_url_with_no_usable_segment_is_refused() {
        for url in [
            "",
            "/",
            "https://github.com/you/..",
            "https://github.com/you/.",
            "https://github.com/you/a b",
        ] {
            let sentence = name_from_url(url).expect_err(url);
            assert!(sentence.contains("names no project"), "{sentence}");
        }
    }

    /// The rule itself, at its two edges: the longest name is one character from refused, and the two
    /// segments that would leave the projects root are refused at any length.
    #[test]
    fn a_name_is_one_path_segment() {
        let longest = "a".repeat(MAX_NAME_CHARS);
        assert!(name_is_valid(&longest));
        assert!(name_is_valid("my.repo_2-x"));
        for name in ["", ".", "..", "a/b", "a b", "proj\nect"] {
            assert!(!name_is_valid(name), "{name:?}");
        }
        assert!(!name_is_valid(&format!("{longest}a")));
    }
}

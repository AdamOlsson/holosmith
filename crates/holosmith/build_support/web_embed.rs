// The one rule that decides what the binary embeds: the built app, or a placeholder.
//
// `build.rs` `include!`s this file and `src/web.rs` `#[path]`-includes it, so the decision
// the build makes and the decision the tests check cannot drift. It depends on nothing, so
// it compiles both as part of the build script and as part of the crate.

/// The fixed string a placeholder page carries, and the string `scripts/check-static.sh`
/// greps for in a shipped artifact. A build that embeds this page is not a release.
pub const PLACEHOLDER_MARKER: &str = "holosmith:web-not-built";

/// What the build should do about the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// `web/dist` is there: embed it.
    Embed,
    /// No `web/dist`: embed a page that says so, loudly, and warn.
    Placeholder,
    /// No `web/dist` and the caller demands the real app: fail the build.
    Refuse,
}

/// Embed the app when it was built; otherwise say so, unless the caller requires it.
///
/// `has_index` is `web/dist/index.html` existing, which is the only evidence of a built app
/// that does not need to trust a directory name.
pub fn decide(has_index: bool, require_web: bool) -> Decision {
    match (has_index, require_web) {
        (true, _) => Decision::Embed,
        (false, true) => Decision::Refuse,
        (false, false) => Decision::Placeholder,
    }
}

/// The placeholder page: a shell that says the web app was not built and how to build it,
/// carrying [`PLACEHOLDER_MARKER`] so no shipped artifact can contain it unnoticed.
pub fn placeholder_index_html() -> String {
    format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\" />\n\
         <title>Holosmith</title>\n\
         <!-- {PLACEHOLDER_MARKER} -->\n\
         </head>\n\
         <body>\n\
         <h1>Holosmith</h1>\n\
         <p>This binary was built without the web app. Run <code>scripts/build-web.sh</code> \
         and build again.</p>\n\
         </body>\n\
         </html>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_built_app_is_always_embedded() {
        assert_eq!(decide(true, false), Decision::Embed);
        assert_eq!(decide(true, true), Decision::Embed);
    }

    #[test]
    fn a_missing_app_is_a_placeholder_unless_it_is_required() {
        assert_eq!(decide(false, false), Decision::Placeholder);
        assert_eq!(decide(false, true), Decision::Refuse);
    }

    #[test]
    fn the_placeholder_names_the_marker_and_the_script() {
        let page = placeholder_index_html();
        assert!(page.contains(PLACEHOLDER_MARKER), "{page}");
        assert!(page.contains("scripts/build-web.sh"), "{page}");
    }
}

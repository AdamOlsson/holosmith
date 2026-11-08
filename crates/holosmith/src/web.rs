//! The embedded web app: the app shell, its assets, and the placeholder that replaces them
//! when the app was not built (ADR-0007).
//!
//! `build.rs` sets `HOLOSMITH_WEB_DIR` to the directory `include_dir!` embeds, which is
//! either the built app or a one-file placeholder carrying [`PLACEHOLDER_MARKER`].
//!
//! `include_dir!` takes a string literal rather than an `env!`, so the invocation itself is
//! generated into `OUT_DIR` by `build.rs` and included here.

// The decision functions in this file are `build.rs`'s half: the crate reads only the marker,
// so they are dead code here and live there. Keeping the module private is what keeps
// `Decision` and `decide` out of the crate's surface.
#[path = "../build_support/web_embed.rs"]
#[allow(dead_code)]
mod web_embed;

pub use web_embed::PLACEHOLDER_MARKER;

include!(concat!(env!("OUT_DIR"), "/web_app.rs"));

/// Served when the embedded index is missing or is not UTF-8: a page that says nothing
/// useful is still better than a panic in a request handler.
const UNREADABLE_INDEX: &str =
    "<!doctype html><title>Holosmith</title><p>The embedded app has no readable index.html.</p>";

/// The shell page: the real app's `index.html`, or the placeholder's.
pub fn index_html() -> &'static str {
    APP.get_file("index.html")
        .and_then(|file| file.contents_utf8())
        .unwrap_or(UNREADABLE_INDEX)
}

/// Whether this binary carries the placeholder instead of the app.
pub fn is_placeholder() -> bool {
    index_html().contains(PLACEHOLDER_MARKER)
}

/// One embedded file and the content type to answer with, if the app has it.
pub fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    let path = path.trim_start_matches('/');
    if path.is_empty() || path.ends_with('/') {
        return None;
    }
    let file = APP.get_file(path)?;
    Some((file.contents(), content_type(path)))
}

/// The content type for the extensions the app ships. Anything else is a byte stream: the
/// set is closed on purpose, so a future asset type arrives with a story and a test.
///
/// `webmanifest` is HS-067's: a manifest served as `application/octet-stream` is a manifest Chrome does not
/// install from, and no browser is available to this build's gate to notice.
pub fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("png") => "image/png",
        Some("webmanifest") => "application/manifest+json",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_extension_has_a_content_type() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("assets/index-BU-f9GoL.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("assets/index-B5Rcbkkx.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type("favicon.svg"), "image/svg+xml");
        assert_eq!(content_type("favicon.ico"), "image/x-icon");
        assert_eq!(content_type("logo.png"), "image/png");
        assert_eq!(
            content_type("/manifest.webmanifest"),
            "application/manifest+json"
        );
        assert_eq!(content_type("font.woff2"), "font/woff2");
    }

    #[test]
    fn an_unknown_extension_or_none_is_a_byte_stream() {
        assert_eq!(content_type("data.json"), "application/octet-stream");
        assert_eq!(content_type("no-extension"), "application/octet-stream");
        assert_eq!(content_type("archive.tar.gz"), "application/octet-stream");
    }

    #[test]
    fn the_app_ships_a_manifest_and_a_worker_in_a_real_build() {
        if std::env::var("HOLOSMITH_REQUIRE_WEB").as_deref() != Ok("1") {
            return;
        }
        // HS-067's installability, as far as the embedded bytes can prove it: the shell names the manifest, the
        // manifest and the worker are files the app carries, and the worker is JavaScript rather than the shell
        // (a `/sw.js` answered by the fallback's shell would register a page as a service worker, and every
        // browser refuses it — silently).
        //
        // The `HOLOSMITH_REQUIRE_WEB` guard is the same one the index test keeps: a tree without `web/dist` serves
        // a placeholder on purpose (ADR-0007). `scripts/verify.sh` exports the variable for its `test` phase when a
        // build is present, so this body runs in the gate; `case_pwa` is the measurement that matters, and this is
        // the cheaper one that fails first.
        let index = index_html();
        assert!(
            index.contains(r#"<link rel="manifest" href="/manifest.webmanifest""#),
            "the shell does not link the manifest: {index}"
        );
        let (manifest, content_type) =
            asset("/manifest.webmanifest").expect("the app has a manifest");
        assert_eq!(content_type, "application/manifest+json");
        assert!(
            std::str::from_utf8(manifest).is_ok_and(|text| text.contains("\"standalone\"")),
            "the manifest does not ask for a standalone display"
        );
        let (worker, content_type) = asset("/sw.js").expect("the app has a service worker");
        assert_eq!(content_type, "text/javascript; charset=utf-8");
        assert!(
            !worker.starts_with(b"<!doctype html"),
            "the worker is the app shell, which every browser refuses to register"
        );
        for icon in [
            "/icons/icon-192.png",
            "/icons/icon-512.png",
            "/icons/icon-512-maskable.png",
            "/icons/apple-touch-icon-180.png",
        ] {
            let (bytes, content_type) = asset(icon).unwrap_or_else(|| panic!("{icon} is missing"));
            assert_eq!(content_type, "image/png", "{icon}");
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "{icon} is not a PNG");
        }
    }

    #[test]
    fn a_trailing_slash_or_an_empty_path_is_not_a_file() {
        assert!(asset("").is_none());
        assert!(asset("/").is_none());
        assert!(asset("assets/").is_none());
        assert!(asset("/assets/").is_none());
    }

    #[test]
    fn a_path_the_app_does_not_have_is_none() {
        assert!(asset("/assets/absent.js").is_none());
    }

    #[test]
    fn the_embedded_index_is_served_and_is_not_a_placeholder_in_a_real_build() {
        let index = index_html();
        assert!(index.contains("<html"), "{index}");
        // A tree without `web/dist` builds a placeholder on purpose (ADR-0007); this test
        // then only proves the placeholder is served like any other index.
        if std::env::var("HOLOSMITH_REQUIRE_WEB").as_deref() == Ok("1") {
            assert!(
                !is_placeholder(),
                "HOLOSMITH_REQUIRE_WEB=1 forbids a placeholder"
            );
        }
    }
}

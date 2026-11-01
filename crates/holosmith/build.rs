// The spec's "build.rs never runs host-side" rule (item 10) governs agent-written
// repo code during promotion: promotion builds run in the builder container. This
// trusted workspace script runs in that same builder, never on the host.
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../pins/pi.txt");
    println!("cargo:rerun-if-env-changed=HOLOSMITH_GIT_TAG");
    println!("cargo:rustc-env=HOLOSMITH_GIT_TAG={}", git_tag());
    println!(
        "cargo:rustc-env=HOLOSMITH_TARGET={}",
        std::env::var("TARGET").expect("cargo sets TARGET for every build script")
    );
}

/// `HOLOSMITH_GIT_TAG` wins (scripts/build.sh sets it, and builder containers have no
/// `.git` to ask); otherwise ask git; a tree with no git answers `unknown` and the
/// build still succeeds.
fn git_tag() -> String {
    if let Ok(from_env) = std::env::var("HOLOSMITH_GIT_TAG") {
        let from_env = from_env.trim();
        if !from_env.is_empty() {
            return from_env.to_string();
        }
    }
    let described = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|tag| !tag.is_empty());
    described.unwrap_or_else(|| "unknown".to_string())
}

//! A root invocation is refused with one sentence, before any verb, subcommand or flag is read (ADR-0090).
//!
//! The decision takes a uid ([`holosmith::identity::root_refusal`]), so both answers are drivable here without a
//! root account: uid 0 gets the sentence, an ordinary uid gets `None`. The verbs are **enumerated from clap's own
//! command** rather than listed here, and that enumeration is the pin this file exists for: `main` asks before
//! `Cli::parse`, so no verb can be exempt, and a verb added later is measured without an edit to this test.

use std::process::Command;

use clap::CommandFactory as _;

use holosmith::cli::Cli;
use holosmith::identity;

/// The sentence a root invocation gets, with the typed words where they belong.
///
/// A second copy of the text on purpose: the operator reads this line, so a change to it has to be a change here.
fn sentence(typed: &str) -> String {
    format!("this runs as you, drop the sudo: holosmith {typed}")
}

/// A readable uid that is not root: no account needs to exist for the decision to be asked about it.
const ORDINARY_UID: u32 = 1000;

#[test]
fn every_verb_is_refused_with_the_same_sentence() {
    // This is the exemption pin, and the verb list is clap's rather than this file's: a verb that gained an
    // exemption would have to be named somewhere else to get one, because the decision is asked before the command
    // line is parsed — so the enumeration is as wide as the build and widens by itself.
    let verbs: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|verb| verb.get_name().to_string())
        .collect();
    assert!(
        !verbs.is_empty(),
        "clap named no verbs, so nothing was measured"
    );
    for verb in &verbs {
        assert_eq!(
            identity::root_refusal(identity::ROOT_UID, verb),
            Some(sentence(verb)),
            "the verb `{verb}` is not refused with the one sentence"
        );
        assert_eq!(
            identity::root_refusal(ORDINARY_UID, verb),
            None,
            "the verb `{verb}` is refused for an ordinary uid"
        );
    }
}

#[test]
fn the_decision_reads_no_verb_and_no_flag() {
    // The decision is a uid and the words that were typed, nothing else: a word that is not a verb, and a flag no
    // command has, are refused exactly like a real verb — which is what "no verb has an exemption, and no flag
    // bypasses the check" means where the enumeration above cannot see.
    for typed in [
        "not-a-verb --force",
        "server deploy --root /x",
        "version --json",
    ] {
        assert_eq!(
            identity::root_refusal(identity::ROOT_UID, typed),
            Some(sentence(typed)),
            "`{typed}` is not refused with the one sentence"
        );
    }
}

#[test]
fn a_bare_invocation_names_no_command() {
    // An operator who typed nothing has nothing to be told to retype: the sentence ends at the program name
    // rather than at a trailing space.
    assert_eq!(
        identity::root_refusal(identity::ROOT_UID, ""),
        Some("this runs as you, drop the sudo: holosmith".to_string())
    );
}

#[test]
fn the_binary_runs_a_verb_as_an_ordinary_account() {
    // The refusal is a refusal of uid 0 and of nothing else: this suite runs as the operator, and `version` is the
    // verb that reads no file and writes none, so it is the one that proves dispatch is still reached.
    let output = Command::new(env!("CARGO_BIN_EXE_holosmith"))
        .arg("version")
        .output()
        .expect("the test binary path from CARGO_BIN_EXE_holosmith runs");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "`holosmith version` refused an ordinary account"
    );
    assert!(
        output.status.success(),
        "`holosmith version` exited {:?}",
        output.status.code()
    );
}

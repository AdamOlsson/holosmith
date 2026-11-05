//! The startup self-test: this machine's isolation, measured rather than asserted (HS-036, spec item 2).
//!
//! Spec item 2 makes the checkbox advisory and the *network* the enforcement: a session's network is
//! `--internal`, so there is no gateway and no route out, and the environment proxies only decide which of the
//! two allowed listeners a container can see. That is a claim about the kernel, and a claim about the kernel
//! that nobody measures is a claim about the daemon's configuration — which is exactly the thing that goes
//! wrong quietly (`--iptables=false`, a bridge that is not internal, a host with `net.ipv4.ip_forward` and a
//! default route). So the same probe a test would write runs at startup:
//!
//! 1. create a **temporary internal network** ([`NETWORK`], reused if a previous run left it),
//! 2. run **one container from the image sessions run** on it, with [`PROBE`] as its command,
//! 3. ask that container to reach the internet and the host, and read its **exit code**,
//! 4. remove the network whatever happened.
//!
//! **Three outcomes, and only one of them refuses.** [`Outcome::Passed`] is nothing reachable;
//! [`Outcome::Failed`] is something reachable, and the boot refuses with a sentence naming the target and the
//! fix — the same shape as `crate::preflight`'s complaints, and reported through the same list.
//! [`Outcome::Unknown`] is "the probe could not run at all" (no image, no daemon that can create a network),
//! which is **not** the same fact as "the machine was measured to leak": it is one `warn` line and the boot
//! continues, because a machine that cannot verify is a machine whose sessions will fail at their own network
//! step with a message about that step. Refusing on `Unknown` would also make every machine whose image is not
//! built yet unable to serve at all, which is not what item 2 asks for.
//!
//! **What a verdict means is in the probe, not here**, and it is the whole test: `connected` and `refused` are
//! *reached* (a reset is proof the packet arrived), and `ENETUNREACH`/`EHOSTUNREACH`/a timeout are not. The
//! measurement on this repository's own machine is why: on the default bridge the host's `172.17.0.1:2375`
//! answers *refused*, and on an internal network it is `ENETUNREACH`. A rule that counted only `connected`
//! would pass a machine whose containers reach the host.
//!
//! Everything here is synchronous, like [`crate::docker`] and [`crate::preflight`], and it runs once before
//! the listener binds. [`Runner`] is the seam: a test decides all three outcomes without a daemon, and
//! [`Docker`] is the one implementation production uses.

use std::fmt;

use crate::docker::Docker;
use crate::preflight::{Check, Complaint};

/// The temporary network the probe runs on. Removed whatever the outcome.
///
/// A name of its own rather than a session's (`agents_<id>`), so an operator who finds it after a crash can
/// tell what it was, and so the orphan sweep HS-044 owns can leave it alone.
pub const NETWORK: &str = "holosmith-selftest";

/// The address that stands for "the internet": a public resolver's HTTPS port, so a successful connection is
/// unambiguous and a refusal still proves a route.
pub const PUBLIC: &str = "1.1.1.1:443";

/// The port the host is probed on.
///
/// The convention for a daemon's unauthenticated TCP API, which is what makes it a *good* canary either way:
/// if something listens, the connection succeeds; if nothing does, the host's stack answers with a reset. Both
/// mean the container reached the host, which is the finding.
pub const HOST_PORT: u16 = 2375;

/// The probe's exit code for "something was reachable".
pub const REACHED: i32 = 9;

/// The probe itself, run by the image's own node (`node -e <script> <targets…>`).
pub const PROBE: &str = include_str!("../templates/selftest-probe.js");

/// What one probe run printed, which is all the decision needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The probe's exit code: [`REACHED`], zero, or something else entirely (a crash, a missing node).
    pub code: i32,
    /// The addresses it reached, comma-separated, on stdout — the probe's own words, used in the sentence.
    pub reached: String,
    /// Its last few stderr lines, for the case where the exit code is neither zero nor [`REACHED`].
    pub complaint: String,
}

/// What the machine did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing was reachable: the isolation is what the spec says it is.
    Passed,
    /// Something was reachable. Carries the sentence the operator reads, which names what and what to fix.
    Failed(String),
    /// The probe could not run. Carries the sentence for the `warn` line — the machine was not measured.
    Unknown(String),
}

impl Outcome {
    /// The complaint this outcome adds to the boot's refusal list, if it adds one.
    ///
    /// Only [`Outcome::Failed`] does: a proven leak must stop the server, and an unmeasured machine must not.
    pub fn complaint(&self) -> Option<Complaint> {
        match self {
            Self::Failed(sentence) => Some(Complaint {
                check: Check::Network,
                sentence: sentence.clone(),
            }),
            Self::Passed | Self::Unknown(_) => None,
        }
    }

    /// Whether the machine was measured and passed.
    pub fn passed(&self) -> bool {
        matches!(self, Self::Passed)
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Passed => f.write_str("passed"),
            Self::Failed(sentence) => write!(f, "failed: {sentence}"),
            Self::Unknown(sentence) => write!(f, "not verified: {sentence}"),
        }
    }
}

/// The machine operations the self-test needs, as a seam.
///
/// One trait so the three outcomes — including the two a well-behaved machine never produces — are decided by
/// a pure function a test can drive, in the spirit of [`crate::preflight`]'s `Tools`. [`Docker`] is the only
/// production implementation.
pub trait Runner {
    /// Create the temporary network, internally, reusing it when it already exists.
    fn create_network(&self, name: &str) -> Result<(), String>;
    /// Remove it, whatever happened. Not being there is not a failure.
    fn remove_network(&self, name: &str);
    /// The addresses a session network must not reach: the internet, and the host when it has one.
    fn targets(&self) -> Vec<String>;
    /// Run the probe on `network` and answer what it reported.
    fn probe(&self, network: &str, image: &str, targets: &[String]) -> Result<Report, String>;
}

impl Runner for Docker {
    fn create_network(&self, name: &str) -> Result<(), String> {
        self.ensure_network(name, true)
            .map_err(|error| error.to_string())
    }

    fn remove_network(&self, name: &str) {
        let _ = Docker::remove_network(self, name);
    }

    fn targets(&self) -> Vec<String> {
        let mut targets = vec![PUBLIC.to_string()];
        // The host, when this daemon's default bridge has a gateway to be reached through. A machine whose
        // daemon reports none (a rootless daemon, or no bridge at all) is not guessed at: the internet target
        // is still measured, and the sentence says which targets were tried.
        if let Some(gateway) = self.gateway("bridge") {
            targets.push(format!("{gateway}:{HOST_PORT}"));
        }
        targets
    }

    fn probe(&self, network: &str, image: &str, targets: &[String]) -> Result<Report, String> {
        let mut arguments = vec![
            "run".to_string(),
            "--rm".to_string(),
            "--network".to_string(),
            network.to_string(),
            image.to_string(),
            "node".to_string(),
            "-e".to_string(),
            PROBE.to_string(),
        ];
        arguments.extend(targets.iter().cloned());
        self.attempt(&arguments).map_err(|error| error.to_string())
    }
}

/// Run the self-test with the machine's own Docker and image.
pub fn run(docker: &Docker, image: &str) -> Outcome {
    run_with(docker, image)
}

/// Run the self-test against any [`Runner`], which is what a test drives.
///
/// The temporary network is created first and removed **whatever happened** — including when the probe could not
/// run — because a network left behind is a name the next boot has to cope with, and an operator has to clean
/// up. Everything else is the decision [`decide`] makes.
pub fn run_with(runner: &dyn Runner, image: &str) -> Outcome {
    if let Err(error) = runner.create_network(NETWORK) {
        return Outcome::Unknown(format!(
            "the probe network could not be created, so this machine's isolation was not measured ({error}); \
             check that the daemon can create networks, and run `docker network create --internal {NETWORK}` \
             by hand to see what it says"
        ));
    }
    let targets = runner.targets();
    let report = runner.probe(NETWORK, image, &targets);
    runner.remove_network(NETWORK);
    match report {
        Ok(report) => decide(&report, &targets),
        Err(error) => Outcome::Unknown(format!(
            "the probe container could not be run, so this machine's isolation was not measured ({error}); \
             check that the image `{image}` exists on this machine and that `docker run` works without a tty"
        )),
    }
}

/// What one probe report means.
///
/// Split out so the three answers are one function of one report — the shape `crate::exit::classify` has, and
/// for the same reason: the decision is the thing worth testing, and the machine is the thing worth not needing.
pub fn decide(report: &Report, targets: &[String]) -> Outcome {
    match report.code {
        0 => Outcome::Passed,
        REACHED => Outcome::Failed(format!(
            "a container on an internal network reached {} — this machine would run sessions that are not \
             isolated: check that the daemon has not been started with `--iptables=false`, that its default \
             bridge is not routable from an internal network, and that no host route answers from one (the \
             targets probed were {})",
            if report.reached.trim().is_empty() {
                "the internet or the host"
            } else {
                report.reached.trim()
            },
            targets.join(", ")
        )),
        code => Outcome::Unknown(format!(
            "the probe exited {code}, so this machine's isolation was not measured: {}{}",
            report.complaint.trim(),
            if report.complaint.trim().is_empty() {
                "it printed nothing"
            } else {
                ""
            }
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runner that answers from a script: what creating the network does, what the probe reports, and
    /// whether it was removed.
    struct Fake {
        create: Result<(), String>,
        probe: Result<Report, String>,
        targets: Vec<String>,
        created: std::cell::RefCell<Vec<String>>,
        removed: std::cell::RefCell<Vec<String>>,
    }

    impl Fake {
        fn passing() -> Self {
            Self {
                create: Ok(()),
                probe: Ok(Report {
                    code: 0,
                    reached: String::new(),
                    complaint: String::new(),
                }),
                targets: vec![PUBLIC.to_string(), format!("172.17.0.1:{HOST_PORT}")],
                created: std::cell::RefCell::new(Vec::new()),
                removed: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl Runner for Fake {
        fn create_network(&self, name: &str) -> Result<(), String> {
            self.created.borrow_mut().push(name.to_string());
            self.create.clone()
        }

        fn remove_network(&self, name: &str) {
            self.removed.borrow_mut().push(name.to_string());
        }

        fn targets(&self) -> Vec<String> {
            self.targets.clone()
        }

        fn probe(
            &self,
            _network: &str,
            _image: &str,
            _targets: &[String],
        ) -> Result<Report, String> {
            self.probe.clone()
        }
    }

    #[test]
    fn nothing_reachable_is_a_pass_and_the_network_is_removed() {
        let fake = Fake::passing();
        let outcome = run_with(&fake, "pi-agent:local");
        assert_eq!(outcome, Outcome::Passed);
        assert_eq!(fake.created.borrow().as_slice(), [NETWORK]);
        assert_eq!(
            fake.removed.borrow().as_slice(),
            [NETWORK],
            "the temporary network must not outlive the test"
        );
        assert!(outcome.complaint().is_none(), "a pass refuses nothing");
        assert!(outcome.passed());
    }

    #[test]
    fn something_reachable_refuses_the_boot_with_a_sentence_that_names_it() {
        let fake = Fake {
            probe: Ok(Report {
                code: REACHED,
                reached: format!("{PUBLIC} connected"),
                complaint: String::new(),
            }),
            ..Fake::passing()
        };
        let outcome = run_with(&fake, "pi-agent:local");
        let Outcome::Failed(sentence) = &outcome else {
            panic!("a reached target must fail the boot: {outcome:?}");
        };
        assert!(
            sentence.contains(PUBLIC),
            "the sentence names what was reached: {sentence}"
        );
        assert!(
            sentence.contains("--iptables=false"),
            "and one thing to check: {sentence}"
        );
        assert!(
            sentence.contains("172.17.0.1:2375"),
            "and the targets it tried, so a partial check is visible: {sentence}"
        );
        // Printed for the story's demo: the sentence is the deliverable for a leaky machine, and a test that
        // asserted it silently would be a demo of a green line.
        eprintln!("a leaking machine refuses the boot with: {sentence}");
        let complaint = outcome.complaint().expect("a leak is a complaint");
        assert_eq!(complaint.check, Check::Network);
        assert_eq!(complaint.check.as_str(), "network");
        assert_eq!(fake.removed.borrow().as_slice(), [NETWORK]);
    }

    #[test]
    fn a_probe_that_could_not_run_is_not_a_refusal_but_says_so() {
        // The distinction the story turns on: "we could not measure" is not "we measured a leak". A machine
        // with no image yet still serves; its sessions fail at their own network step with a better sentence.
        for report in [
            Err("docker run: no such image".to_string()),
            Ok(Report {
                code: 1,
                reached: String::new(),
                complaint: "node: not found".to_string(),
            }),
            Ok(Report {
                code: 137,
                reached: String::new(),
                complaint: String::new(),
            }),
        ] {
            let fake = Fake {
                probe: report.clone(),
                ..Fake::passing()
            };
            let outcome = run_with(&fake, "missing:image");
            let Outcome::Unknown(sentence) = &outcome else {
                panic!("{report:?} must be unknown, not a verdict: {outcome:?}");
            };
            assert!(
                sentence.contains("not measured"),
                "the sentence says what did not happen: {sentence}"
            );
            assert!(outcome.complaint().is_none(), "{sentence}");
            assert!(!outcome.passed());
            assert_eq!(
                fake.removed.borrow().as_slice(),
                [NETWORK],
                "a probe that could not run must not leave its network behind"
            );
        }
    }

    #[test]
    fn a_network_that_cannot_be_created_is_unknown_and_nothing_is_removed() {
        let fake = Fake {
            create: Err("Error response from daemon: could not create network".to_string()),
            ..Fake::passing()
        };
        let outcome = run_with(&fake, "pi-agent:local");
        let Outcome::Unknown(sentence) = &outcome else {
            panic!("{outcome:?}");
        };
        assert!(sentence.contains("could not be created"), "{sentence}");
        assert!(
            fake.removed.borrow().is_empty(),
            "there was nothing to remove, and removing a network this run did not create would be a lie"
        );
        assert!(fake.created.borrow().as_slice() == [NETWORK]);
    }

    #[test]
    fn the_three_verdicts_are_one_function_of_one_report() {
        // The decision, without a runner: the probe's exit code is the whole input.
        let targets = vec![PUBLIC.to_string()];
        let report = |code: i32, reached: &str| Report {
            code,
            reached: reached.to_string(),
            complaint: String::new(),
        };
        assert_eq!(decide(&report(0, ""), &targets), Outcome::Passed);
        assert!(matches!(
            decide(&report(REACHED, "x:1 refused"), &targets),
            Outcome::Failed(_)
        ));
        // The probe's own protocol is 9; anything else is a probe that did not finish, which is unknown.
        for code in [1, 2, 126, 127, 137] {
            assert!(
                matches!(decide(&report(code, ""), &targets), Outcome::Unknown(_)),
                "{code}"
            );
        }
        // A refused connection is a *reached* target, and the sentence says so in the probe's own words.
        let Outcome::Failed(sentence) =
            decide(&report(REACHED, "172.17.0.1:2375 refused"), &targets)
        else {
            panic!("a refusal is a reach");
        };
        assert!(sentence.contains("refused"), "{sentence}");
    }

    #[test]
    fn the_probe_is_a_script_that_reports_by_exit_code() {
        // The template is embedded, so a probe that lost its verdict would be a compile-time change rather than
        // a silent one. Both the pass and the finding must be in it.
        assert!(PROBE.contains("REACHED_EXIT = 9"), "the finding's code");
        assert!(PROBE.contains("process.exit(0)"), "the pass's exit code");
        assert!(
            PROBE.contains("ECONNREFUSED"),
            "a refused connection is a reached host, and that is the distinction the test rests on"
        );
        assert!(
            PROBE.contains("ENETUNREACH") || PROBE.contains("error.code"),
            "an unreachable network is the pass, and it is decided by the error code"
        );
    }
}

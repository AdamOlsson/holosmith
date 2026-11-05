//! The startup self-test: this machine's isolation, measured rather than asserted (HS-036, spec item 2).
//!
//! Spec item 2 makes the checkbox advisory and the *network* the enforcement: a session's network is
//! `--internal`, so there is no route out, and the environment proxies only decide which of the two allowed
//! listeners a container can see. ("No route out" and not "no gateway": an internal network still reports an
//! IPAM gateway, and that address answers from inside it — measured, ADR-0044. What it cannot do is route
//! anywhere else, which is what the probe below asks.) That is a claim about the kernel, and a claim about the kernel
//! that nobody measures is a claim about the daemon's configuration — which is exactly the thing that goes
//! wrong quietly (`--iptables=false`, a bridge that is not internal, a host with `net.ipv4.ip_forward` and a
//! default route). So the same probe a test would write runs at startup:
//!
//! 1. create a **temporary internal network** ([`NETWORK`], reused if a previous run left it),
//! 2. run **one container from the image sessions run** on it, with [`PROBE`] as its command,
//! 3. ask that container to reach the internet, the host **and the egress proxy's alias**, and read its **exit
//!    code** — nothing may answer, which is spec item 2's off case (HS-038),
//! 4. join the machine's egress proxy to that same network and ask again, about the alias only: it must answer
//!    now, or the first measurement would have been silence for the wrong reason (an alias that never resolves
//!    passes a check for a proxy that is not there at all),
//! 5. remove the network whatever happened.
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

/// The address the egress proxy answers on from a session's network — and therefore the address that must **not**
/// answer on a network whose session asked for no internet access (HS-038, spec item 2's off case).
///
/// The alias a session's container resolves the proxy by, not the proxy's container name
/// ([`crate::container::PROXY_ALIAS`]): the alias is what Docker's embedded DNS answers, so a name that does not
/// resolve is proof that nothing is attached — measured on this machine as `EAI_AGAIN` on a network the proxy is
/// not joined to, against `connected` on one where it is. That is the cleanest "nothing answers" there is, and it
/// is the only fact that tells "off" from "the proxy is missing by accident".
///
/// **The key proxy is not here yet.** HS-039 adds it, and it belongs in this list: the off case's promise is that
/// only the key proxy answers. A target nothing answers today would prove nothing, which is why it is one entry
/// and not two with a comment.
pub const PROXY_TARGET: &str = "egress-proxy:3128";

/// The key proxy's alias, which **must answer** on a session network (HS-039, spec item 2).
///
/// It is deliberately not in [`Runner::targets`]: that list is things nothing may reach. This is the one
/// legitimate answer on a session network whose Internet access is off, so it is the second half of the pair:
/// join `hs_key`, ask for this alias alone, and require a connection. A build whose network step forgot the
/// join is a model call that fails at once, and this is where the boot says so.
pub const KEY_PROXY_TARGET: &str = "key-proxy:8080";

/// The port the host is probed on.
///
/// The convention for a daemon's unauthenticated TCP API, which is what makes it a *good* canary either way:
/// if something listens, the connection succeeds; if nothing does, the host's stack answers with a reset. Both
/// mean the container reached the host, which is the finding.
pub const HOST_PORT: u16 = 2375;

/// The probe's exit code for "something was reachable".
pub const REACHED: i32 = 9;

/// How long the probe's `docker run` may take before it is killed and the outcome becomes
/// [`Outcome::Unknown`].
///
/// The probe bounds itself (three seconds per target, and a run asks about at most three or one), so this is a
/// bound on each *container*, not on the test: a daemon that accepts a run and then wedges must not hang a boot
/// before the listener binds —
/// the same hazard `crate::preflight`'s `PING_TIMEOUT` names for its own check. Generous rather than tight,
/// because a slow machine image unpacking for the first time is not a wedged one.
pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

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
    /// The addresses a session network must not reach: the internet, the egress proxy's alias, and the host
    /// when the daemon has one.
    fn targets(&self) -> Vec<String>;
    /// Run the probe on `network` and answer what it reported.
    fn probe(&self, network: &str, image: &str, targets: &[String]) -> Result<Report, String>;
    /// The egress proxy's container name, when this machine has one running. `None` is a machine that runs
    /// sessions without a proxy (HS-037 warns about it rather than refusing), and it is why the pair below is not
    /// measured — not a verdict about isolation.
    fn proxy(&self) -> Option<String>;
    /// Attach the proxy to `network` under the alias a session's container resolves it by.
    fn join_proxy(&self, network: &str) -> Result<(), String>;
    /// Detach it again. Not being attached is not a failure.
    fn leave_proxy(&self, network: &str);

    /// The key proxy's container name, when this machine has one running.
    fn key_proxy(&self) -> Option<String> {
        None
    }

    /// Attach the key proxy to `network` under its alias.
    fn join_key_proxy(&self, _network: &str) -> Result<(), String> {
        Err("this runner has no key proxy".to_string())
    }

    /// Detach it again.
    fn leave_key_proxy(&self, _network: &str) {}
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
        // The proxy is asked about first only in this comment: the probe asks all of them at once, and a proxy
        // that answers is `connected` — reached, by the rule the whole test rests on.
        let mut targets = vec![PUBLIC.to_string(), PROXY_TARGET.to_string()];
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
        match self.attempt(&arguments, PROBE_TIMEOUT) {
            Ok(Some(report)) => Ok(report),
            Ok(None) => Err(format!(
                "the probe did not finish within {} seconds and was killed",
                PROBE_TIMEOUT.as_secs()
            )),
            Err(error) => Err(error.to_string()),
        }
    }

    fn proxy(&self) -> Option<String> {
        self.running(crate::container::PROXY_NAME)
            .then(|| crate::container::PROXY_NAME.to_string())
    }

    fn join_proxy(&self, network: &str) -> Result<(), String> {
        self.join_network(
            network,
            crate::container::PROXY_NAME,
            crate::container::PROXY_ALIAS,
        )
        .map_err(|error| error.to_string())
    }

    fn leave_proxy(&self, network: &str) {
        self.leave_network(network, crate::container::PROXY_NAME);
    }

    fn key_proxy(&self) -> Option<String> {
        self.running(crate::key::KEY_PROXY_NAME)
            .then(|| crate::key::KEY_PROXY_NAME.to_string())
    }

    fn join_key_proxy(&self, network: &str) -> Result<(), String> {
        self.join_network(
            network,
            crate::key::KEY_PROXY_NAME,
            crate::key::KEY_PROXY_ALIAS,
        )
        .map_err(|error| error.to_string())
    }

    fn leave_key_proxy(&self, network: &str) {
        self.leave_network(network, crate::key::KEY_PROXY_NAME);
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
    // Spec item 2's **off case**, measured first: on a network nothing is attached to, none of the targets may
    // answer — the internet, the host, or the egress proxy's own alias.
    let report = runner.probe(NETWORK, image, &targets);
    // …and then the same network with the proxy on it, before the network goes. This is what makes the silence
    // above mean something: an alias that never resolves would pass the off case for a proxy that is not there
    // at all, which is the bug the off case exists to catch (HS-038).
    pair(runner, image);
    key_pair(runner, image);
    runner.remove_network(NETWORK);
    match report {
        Ok(report) => decide(&report, &targets),
        Err(error) => Outcome::Unknown(format!(
            "the probe container could not be run, so this machine's isolation was not measured ({error}); \
             check that the image `{image}` exists on this machine and that `docker run` works without a tty"
        )),
    }
}

/// The other half of the off case: join the machine's egress proxy to the probe's network, ask the probe about
/// **nothing but the alias**, and leave again.
///
/// Not a verdict, and deliberately so. A proxy that will not join, or that is not built on this machine, is
/// HS-037's `warn` and not a statement about isolation — refusing the boot over it would take the control UI down
/// for a machine whose sessions otherwise work. What it buys is the *discrimination*: the off case's silence is
/// only evidence if the same network reports the proxy the moment it is attached, and no other check in the build
/// puts the two facts side by side at startup.
///
/// The join is `join_network`'s (disconnect, then connect with the alias), so a proxy left attached by a previous
/// run is replaced rather than doubled; the leave is unconditional, and the network is removed right after.
fn pair(runner: &dyn Runner, image: &str) {
    let Some(proxy) = runner.proxy() else {
        tracing::debug!(
            target: "holosmith::selftest",
            proxy = crate::container::PROXY_NAME,
            "the egress proxy is not running on this machine, so the off case's pair — the proxy answering once \
             it is joined — was not measured"
        );
        return;
    };
    let one = [PROXY_TARGET.to_string()];
    let joined = runner
        .join_proxy(NETWORK)
        .map_err(|error| format!("the proxy could not be joined to the probe's network ({error})"))
        .and_then(|()| runner.probe(NETWORK, image, &one));
    runner.leave_proxy(NETWORK);
    match joined {
        Ok(report) if report.code == REACHED => tracing::debug!(
            target: "holosmith::selftest",
            proxy = %proxy,
            "the probe sees the egress proxy once it is joined, which is what makes the off case's silence mean \
             something"
        ),
        Ok(report) => tracing::warn!(
            target: "holosmith::selftest",
            proxy = %proxy,
            reached = %report.reached.trim(),
            complaint = %report.complaint.trim(),
            "the egress proxy was joined to the probe's network and nothing answered on the alias, so a session \
             that asked for internet access would have a word and no route"
        ),
        Err(error) => tracing::warn!(
            target: "holosmith::selftest",
            %error,
            "the egress proxy's half of the self-test could not be measured: a session's internet access may be a \
             word rather than a route"
        ),
    }
}

/// The key proxy's half of the off case: join it to the probe's network, ask for its alias alone, and require
/// **an answer**.
///
/// This is the inverse of [`pair`] and it is not a verdict either. A key proxy that is missing is HS-039's
/// `warn`, not a machine that leaks; what the line buys is the discrimination "the alias resolves on a session
/// network" that no unit test can make at boot.
fn key_pair(runner: &dyn Runner, image: &str) {
    let Some(proxy) = runner.key_proxy() else {
        tracing::debug!(
            target: "holosmith::selftest",
            proxy = crate::key::KEY_PROXY_NAME,
            "the key proxy is not running, so the off case's legitimate answer — the key proxy's alias — was              not measured"
        );
        return;
    };
    let one = [KEY_PROXY_TARGET.to_string()];
    let joined = runner
        .join_key_proxy(NETWORK)
        .map_err(|error| {
            format!("the key proxy could not be joined to the probe's network ({error})")
        })
        .and_then(|()| runner.probe(NETWORK, image, &one));
    runner.leave_key_proxy(NETWORK);
    match joined {
        Ok(report) if report.code == REACHED => tracing::debug!(
            target: "holosmith::selftest",
            proxy = %proxy,
            "the key proxy answers on the probe's network, which is the one address a session without internet              access may reach"
        ),
        Ok(report) => tracing::warn!(
            target: "holosmith::selftest",
            proxy = %proxy,
            reached = %report.reached.trim(),
            complaint = %report.complaint.trim(),
            "the key proxy was joined to the probe's network and its alias did not answer, so a session's model              calls will fail"
        ),
        Err(error) => tracing::warn!(
            target: "holosmith::selftest",
            %error,
            "the key proxy's half of the self-test could not be measured: a session's model path may be a word              rather than a route"
        ),
    }
}

/// What one probe report means.
///
/// Split out so the three answers are one function of one report — the shape `crate::exit::classify` has, and
/// for the same reason: the decision is the thing worth testing, and the machine is the thing worth not needing.
pub fn decide(report: &Report, targets: &[String]) -> Outcome {
    match report.code {
        0 => Outcome::Passed,
        // Two findings, two sentences: a proxy that answers where nothing should leave is a wiring mistake
        // (the session's `network` flag not reaching the topology), and one sentence that named both would be a
        // checklist instead of the one thing to go and look at. A direct leak is the worse answer and wins.
        REACHED if !leaked(report) => Outcome::Failed(format!(
            "the egress proxy answered on the probe's internal network ({}) — sessions that asked for no internet \
             access would reach it anyway: check that nothing joins {} to a network at startup and that a session's \
             network is joined only when its row says it may be; `docker network inspect <network>` lists what \
             is attached",
            report.reached.trim(),
            crate::container::PROXY_NAME,
        )),
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

/// Whether anything other than the egress proxy was reached — the question that decides which of the two
/// `REACHED` sentences an operator reads.
///
/// The report's `reached` field is the probe's own words: `target verdict` pairs, comma-separated, in the order the
/// probe printed them. The target is everything before the last space, because a target is `host:port` and a
/// verdict is one word (`connected`, `refused`, `timed out`). A report that printed nothing counts as a leak: a
/// probe that answered [`REACHED`] without naming a target is not a report this build can read, and the safe
/// reading of an unreadable finding is the worse one.
fn leaked(report: &Report) -> bool {
    let targets: Vec<&str> = report
        .reached
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| match entry.rsplit_once(' ') {
            Some((target, _verdict)) => target,
            None => entry,
        })
        .collect();
    targets.is_empty() || targets.iter().any(|target| *target != PROXY_TARGET)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runner that answers from a script: what creating the network does, **what each probe run reports** (a
    /// queue, because the off case runs one probe and its pair runs a second), whether the proxy was joined, and
    /// whether the network was removed.
    struct Fake {
        create: Result<(), String>,
        probes: std::cell::RefCell<std::collections::VecDeque<Result<Report, String>>>,
        proxy: Option<String>,
        join: Result<(), String>,
        targets: Vec<String>,
        created: std::cell::RefCell<Vec<String>>,
        removed: std::cell::RefCell<Vec<String>>,
        joined: std::cell::RefCell<Vec<String>>,
        left: std::cell::RefCell<Vec<String>>,
        asked: std::cell::RefCell<Vec<Vec<String>>>,
    }

    impl Fake {
        fn passing() -> Self {
            Self {
                create: Ok(()),
                probes: std::cell::RefCell::new(
                    [Ok(Report {
                        code: 0,
                        reached: String::new(),
                        complaint: String::new(),
                    })]
                    .into_iter()
                    .collect(),
                ),
                proxy: None,
                join: Ok(()),
                // The production list, to the letter: the fake is asked the same questions the machine is, or
                // the assertions about which targets are probed would be about a list nobody builds.
                targets: vec![
                    PUBLIC.to_string(),
                    PROXY_TARGET.to_string(),
                    format!("172.17.0.1:{HOST_PORT}"),
                ],
                created: std::cell::RefCell::new(Vec::new()),
                removed: std::cell::RefCell::new(Vec::new()),
                joined: std::cell::RefCell::new(Vec::new()),
                left: std::cell::RefCell::new(Vec::new()),
                asked: std::cell::RefCell::new(Vec::new()),
            }
        }

        /// The same runner, with the machine's proxy running and a second scripted probe answer.
        fn paired(second: Report) -> Self {
            let fake = Self {
                proxy: Some(crate::container::PROXY_NAME.to_string()),
                ..Self::passing()
            };
            fake.probes.borrow_mut().push_back(Ok(second));
            fake
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
            targets: &[String],
        ) -> Result<Report, String> {
            self.asked.borrow_mut().push(targets.to_vec());
            match self.probes.borrow_mut().pop_front() {
                Some(answer) => answer,
                // A run that asks more than it scripted is a bug in this test, not a machine fact.
                None => panic!("the fake was asked for more probe runs than it was given"),
            }
        }

        fn proxy(&self) -> Option<String> {
            self.proxy.clone()
        }

        fn join_proxy(&self, network: &str) -> Result<(), String> {
            self.joined.borrow_mut().push(network.to_string());
            self.join.clone()
        }

        fn leave_proxy(&self, network: &str) {
            self.left.borrow_mut().push(network.to_string());
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
        let fake = Fake::passing();
        *fake
            .probes
            .borrow_mut()
            .front_mut()
            .expect("the fake was given one scripted probe") = Ok(Report {
            code: REACHED,
            reached: format!("{PUBLIC} connected"),
            complaint: String::new(),
        });
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
            let fake = Fake::passing();
            *fake
                .probes
                .borrow_mut()
                .front_mut()
                .expect("the fake was given one scripted probe") = report.clone();
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

    /// HS-038's off case, as three reports. The proxy answering where nothing should leave is its **own**
    /// finding — a different fix from a leaking bridge — and a direct leak alongside it is the worse of the two,
    /// so it keeps the isolation sentence.
    #[test]
    fn a_proxy_on_a_no_internet_network_is_its_own_finding() {
        let targets = vec![
            PUBLIC.to_string(),
            PROXY_TARGET.to_string(),
            format!("172.17.0.1:{HOST_PORT}"),
        ];
        let report = |reached: &str| Report {
            code: REACHED,
            reached: reached.to_string(),
            complaint: String::new(),
        };

        // Only the proxy answered: the sentence names the proxy and the fix is the join, not the bridge.
        let Outcome::Failed(sentence) =
            decide(&report(&format!("{PROXY_TARGET} connected")), &targets)
        else {
            panic!("a proxy that answers on a network that asked for no internet is a finding");
        };
        assert!(
            sentence.contains(crate::container::PROXY_NAME),
            "the sentence names the container to go and look at: {sentence}"
        );
        assert!(
            sentence.contains(PROXY_TARGET),
            "and the address that answered: {sentence}"
        );
        assert!(
            !sentence.contains("--iptables=false"),
            "a bridge hint would send the operator to the wrong machine setting: {sentence}"
        );

        // The proxy *and* the internet: the leak outranks the wiring, so the isolation sentence is the one.
        let Outcome::Failed(sentence) = decide(
            &report(&format!("{PUBLIC} connected,{PROXY_TARGET} connected")),
            &targets,
        ) else {
            panic!("a reached public address is always a finding");
        };
        assert!(sentence.contains("--iptables=false"), "{sentence}");
        assert!(sentence.contains(PUBLIC), "{sentence}");

        // Nothing reached at all is the pass, proxy target or not: the check must not turn every boot red.
        assert_eq!(
            decide(
                &Report {
                    code: 0,
                    reached: String::new(),
                    complaint: String::new(),
                },
                &targets,
            ),
            Outcome::Passed
        );
    }

    /// The pair is what makes the off case evidence rather than a tautology: the *same* network reports the proxy
    /// once it is joined, so a silence on a network nothing is attached to cannot be the silence of an alias that
    /// never resolves.
    #[test]
    fn the_proxy_is_asked_about_again_on_the_same_network_once_it_is_joined() {
        let fake = Fake::paired(Report {
            code: REACHED,
            reached: format!("{PROXY_TARGET} connected"),
            complaint: String::new(),
        });
        let outcome = run_with(&fake, "pi-agent:local");
        assert_eq!(outcome, Outcome::Passed);
        assert_eq!(
            fake.asked.borrow().len(),
            2,
            "the off case and its pair: {:?}",
            fake.asked.borrow()
        );
        assert!(
            fake.asked.borrow()[0].contains(&PROXY_TARGET.to_string()),
            "the first run asks about the proxy along with everything else: {:?}",
            fake.asked.borrow()[0]
        );
        assert_eq!(
            fake.asked.borrow()[1].as_slice(),
            [PROXY_TARGET.to_string()],
            "the second asks about the alias alone, so its answer cannot be another target's"
        );
        assert_eq!(fake.joined.borrow().as_slice(), [NETWORK]);
        assert_eq!(
            fake.left.borrow().as_slice(),
            [NETWORK],
            "the proxy must not stay on the probe's network"
        );
        assert_eq!(
            fake.removed.borrow().as_slice(),
            [NETWORK],
            "and the network must not outlive the run either"
        );
    }

    #[test]
    fn a_proxy_that_does_not_answer_the_pair_is_not_a_verdict_about_isolation() {
        // Joined and silent: the machine's proxy is broken, which HS-037 makes a `warn` — it fails a download,
        // not a session — so the isolation is still `Passed` and nothing is refused. The join and the leave still
        // happened, or a machine whose proxy is fine would drift into this arm unscathed.
        let fake = Fake::paired(Report {
            code: 0,
            reached: String::new(),
            complaint: String::new(),
        });
        let outcome = run_with(&fake, "pi-agent:local");
        assert_eq!(outcome, Outcome::Passed);
        assert!(outcome.complaint().is_none(), "{outcome:?}");
        assert_eq!(fake.joined.borrow().as_slice(), [NETWORK]);
        assert_eq!(fake.left.borrow().as_slice(), [NETWORK]);
    }

    #[test]
    fn without_a_proxy_the_pair_is_not_measured_and_nothing_is_joined() {
        // `Fake::passing()` has no proxy running: the second run would be a second silence, which is not evidence,
        // so it is not made — and a join of a container that is not there would be a `warn` at every boot.
        let fake = Fake::passing();
        assert_eq!(run_with(&fake, "pi-agent:local"), Outcome::Passed);
        assert_eq!(fake.asked.borrow().len(), 1);
        assert!(fake.joined.borrow().is_empty());
        assert!(fake.left.borrow().is_empty());
    }

    #[test]
    fn an_unreadable_reached_set_is_the_worse_reading() {
        // `REACHED` with nothing printed cannot be attributed, and "something reached something" is not a pass.
        // The point of the assertion is that the empty set falls through to neither the proxy sentence nor
        // `Passed`: an unattributed finding is still a finding.
        let targets = vec![PUBLIC.to_string(), PROXY_TARGET.to_string()];
        let Outcome::Failed(sentence) = decide(
            &Report {
                code: REACHED,
                reached: String::new(),
                complaint: String::new(),
            },
            &targets,
        ) else {
            panic!("an unattributed finding is a finding");
        };
        assert!(sentence.contains("the internet or the host"), "{sentence}");
    }

    #[test]
    fn the_probe_is_asked_about_the_proxy_by_its_alias() {
        // The target is the *alias* Docker's embedded DNS answers on a network the proxy is joined to
        // (`egress-proxy`), never the container's name (`hs_egress`): the container's name resolves on the
        // `egress` network only, so a probe asking for it would pass on every machine, joined or not.
        assert_eq!(
            PROXY_TARGET,
            format!("{}:{}", crate::container::PROXY_ALIAS, crate::egress::PORT),
            "the probe's target is the address the container's own `HTTPS_PROXY` carries"
        );
        assert!(
            !PROXY_TARGET.contains(crate::container::PROXY_NAME),
            "the alias and the container name are two strings, and the probe asks the alias"
        );
        // The real machine's list is built from the same constant, so the production runner cannot drift from
        // the sentence. `.targets()` needs no daemon: its host half shells out to `docker network inspect` and
        // answers nothing when that fails, which is why only the two fixed targets are asserted here.
        let docker = Docker::new(None);
        let targets = docker.targets();
        assert!(targets.contains(&PUBLIC.to_string()), "{targets:?}");
        assert!(targets.contains(&PROXY_TARGET.to_string()), "{targets:?}");
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

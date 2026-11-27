// The Service screen's rules (ADR-0110), tested without a browser.
//
// `npm test` is the only gate that can see a sentence, and no gate may need a browser, so everything the screen
// decides is a pure function in `web/src/service.ts` and is asserted here: the four states the section can be in (a
// machine that has promoted nothing, a job under way, a job that ended and left the machine draining, a job whose
// gate refused), the five reasons the button is withheld, the wait the page says before the press, the rule that a
// report written before the page's own `202` is the **previous** run, and the body a press sends.
//
// Run with `npm test` in `web/`: node 24's own runner, which strips the types. Like the other suites this file sits
// outside `src`, so `tsc --noEmit` does not typecheck it — but the gate runs it, and the extension in the
// `../src/service.ts` import is what node's ESM resolver requires.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  CANDIDATE_PATH,
  JOB_LINES_SHOWN,
  JOB_ACCEPTED,
  JOB_RUNNING,
  POLL_INTERVAL_MS,
  PROMOTE_DRAINING,
  PROMOTE_HUB,
  PROMOTE_LABEL,
  PROMOTE_RUNNING,
  PROMOTE_WAIT,
  SERVICE_CARD_SENTENCE,
  UPDATE_PATH,
  candidateFacts,
  candidateText,
  detailText,
  drainText,
  endingText,
  gateText,
  isThisRun,
  jobHead,
  jobLines,
  jobRunning,
  liveText,
  promoteBody,
  promoteOffered,
  refusalSentence,
  type Candidate,
  type Report,
} from "../src/service.ts";
import { SERVICE_PATH } from "../src/push.ts";

// The commit a promotion would make live, as the candidate route answers it.
const CANDIDATE: Candidate = {
  source: "/home/operator/holosmith-src",
  commit: "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c",
  tag: "v0.1.0",
};

// A job that never ran: what `GET /api/update` refuses with before a promotion, as the page holds it. Every state
// below is a report, and this is the one that is not.
const NOTHING: Report | null = null;

// The report of a job under way, as the job's own seed writes it: `running`, dated, and nothing claimed.
function running(since = 1_700_000_000, lines: string[] = []): Report {
  return {
    state: "running",
    since,
    gate: null,
    detail: null,
    log: { path: "/home/operator/.local/share/holosmith/update.log", lines },
  };
}

// The gate's record: `ending` is the report's word, `skips` the cases that measured nothing.
function gate(overrides: Partial<Report["gate"] & object> = {}) {
  return {
    command: "/home/operator/holosmith-src/scripts/verify-in-container.sh",
    image: "holosmith-base:0.1.0",
    exit_code: 1,
    ending: "refused",
    skips: 0,
    ...overrides,
  };
}

// The report of a job that ended with the state and the sentence the test names.
function ended(state: string, overrides: Partial<Report> = {}): Report {
  return {
    state,
    since: 1_700_000_000,
    gate: null,
    detail: null,
    ...overrides,
  };
}

// ---- idle: no job, and the button ------------------------------------------------

test("a machine that has promoted nothing says so and offers the button", () => {
  // The `404 not_found` before any run is a `null` report here (`no_promotion`, the route's own sentence). There is
  // no job heading, no lines and no ending: the screen is the version, the candidate, the wait and the button.
  assert.equal(jobHead(false, NOTHING), null);
  assert.deepEqual(jobLines(NOTHING), []);
  assert.equal(endingText(NOTHING), null);
  assert.equal(detailText(NOTHING), null);

  const offered = promoteOffered({
    role: "server",
    draining: false,
    running: false,
    candidate: candidateFacts({ ok: true, value: CANDIDATE }),
  });
  assert.deepEqual(offered, { allowed: true, sentence: null });
});

test("the screen names the live version and the candidate a press would send", () => {
  assert.equal(liveText("0.1.0"), "the live version is 0.1.0");
  assert.equal(drainText(false), null);
  const sentence = candidateText(candidateFacts({ ok: true, value: CANDIDATE }));
  // `master`'s commit **and** its tag, and the cleanliness a candidate's existence implies: a dirty tree is the
  // route's refusal, not a candidate with a flag.
  assert.ok(sentence.includes(CANDIDATE.commit), sentence);
  assert.ok(sentence.includes(CANDIDATE.tag), sentence);
  assert.ok(sentence.includes("clean"), sentence);
});

test("the wait is said before the press, and it names what the button does", () => {
  // 45 minutes with no warning is the wrong surprise: the sentence is on the screen before the button is pressed, and
  // it says both what runs and how long it takes.
  assert.ok(PROMOTE_WAIT.includes("about 45 minutes"), PROMOTE_WAIT);
  assert.ok(PROMOTE_WAIT.includes("gate"), PROMOTE_WAIT);
  assert.ok(PROMOTE_WAIT.includes("live"), PROMOTE_WAIT);
  assert.ok(!PROMOTE_WAIT.includes("\n"), "the sentence is one sentence");
  assert.equal(PROMOTE_LABEL, "Promote");
});

// ---- the four reasons the button is withheld ---------------------------------------

test("a job under way withholds the button and says the job is the reason", () => {
  const offered = promoteOffered({
    role: "server",
    draining: false,
    running: true,
    candidate: candidateFacts({ ok: true, value: CANDIDATE }),
  });
  assert.deepEqual(offered, { allowed: false, sentence: PROMOTE_RUNNING });
});

test("a draining machine withholds the button even with a candidate in hand", () => {
  const offered = promoteOffered({
    role: "server",
    draining: true,
    running: false,
    candidate: candidateFacts({ ok: true, value: CANDIDATE }),
  });
  assert.deepEqual(offered, { allowed: false, sentence: PROMOTE_DRAINING });
});

test("a hub is refused as a hub, whatever its candidate read answered", () => {
  // The role comes first: "there is no candidate" about a hub would be a smaller fact than the reason, and the route
  // refuses that machine for the same reason (`hub_cannot_promote`).
  const offered = promoteOffered({
    role: "hub",
    draining: false,
    running: false,
    candidate: candidateFacts({ ok: false, code: "update_disabled", message: "a hub promotes nothing" }),
  });
  assert.deepEqual(offered, { allowed: false, sentence: PROMOTE_HUB });
});

test("no candidate withholds the button with the route's own sentence", () => {
  // `no_candidate` and `update_disabled` are a press's answers too, and each names its repair — so the page prints
  // the API's sentence rather than a second one that could drift from it.
  const noCandidate = "this machine has no candidate: … point that key at the checkout with `holosmith config edit`";
  assert.deepEqual(
    promoteOffered({
      role: "server",
      draining: false,
      running: false,
      candidate: candidateFacts({ ok: false, code: "no_candidate", message: noCandidate }),
    }),
    { allowed: false, sentence: noCandidate },
  );
  const disabled = "this machine's `[update] enabled` config key is false, so no promotion may be started";
  assert.deepEqual(
    promoteOffered({
      role: "server",
      draining: false,
      running: false,
      candidate: candidateFacts({ ok: false, code: "update_disabled", message: disabled }),
    }),
    { allowed: false, sentence: disabled },
  );

  // A refusal whose code the page has nothing to add to keeps it in front, as `hostFailure` does.
  assert.equal(
    refusalSentence({ code: "unreachable", sentence: "TypeError: fetch failed" }),
    "unreachable: TypeError: fetch failed",
  );
  assert.equal(refusalSentence({ code: "no_candidate", sentence: noCandidate }), noCandidate);
});

// ---- running: the job's lines -------------------------------------------------------

test("a running job is its own heading and its own last lines", () => {
  const report = running(1_700_000_000, ["update: candidate: 0f1e2d at v0.1.0", "update: gate: started"]);
  assert.equal(jobRunning(report), true);
  assert.equal(jobHead(false, report), JOB_RUNNING);
  assert.deepEqual(jobLines(report), [
    "update: candidate: 0f1e2d at v0.1.0",
    "update: gate: started",
  ]);
  // Nothing has ended, so the ending says nothing: a state that has not happened must not be announced.
  assert.equal(endingText(report), null);
  assert.equal(detailText(report), null);
});

test("the lines are the log's last few, and a job with none is no lines", () => {
  const many = Array.from({ length: JOB_LINES_SHOWN + 3 }, (_, index) => `line ${index}`);
  const shown = jobLines(running(1_700_000_000, many));
  assert.equal(shown.length, JOB_LINES_SHOWN);
  assert.equal(shown[shown.length - 1], `line ${JOB_LINES_SHOWN + 2}`);
  assert.deepEqual(jobLines(running()), []);
});

test("a press that no report answers yet is the accepted sentence, not the last run's ending", () => {
  // The window `http/update.rs` names: the `202` carries the instant the page pressed, and the report still holds the
  // previous promotion — so the page says the job was accepted and keeps waiting.
  assert.equal(jobHead(true, ended("live")), JOB_ACCEPTED);
  assert.equal(JOB_ACCEPTED.includes("accepted"), true);
});

// ---- ended, drained: a failure leaves the machine draining --------------------------

test("a failed promotion says the previous release is running, names the gate's ending and the drain", () => {
  const report = ended("failed", {
    gate: gate({ exit_code: 1, ending: "refused", skips: 0 }),
    detail: "the promotion ran and the new process did not answer `/api/health` within twenty seconds",
  });
  assert.equal(jobRunning(report), false);
  assert.equal(jobHead(false, report), null, "an ending is its own sentence");
  const sentence = endingText(report);
  assert.ok(sentence !== null, "a failed promotion has no ending sentence");
  assert.ok(sentence.includes("previous release"), sentence);
  // The gate's own record, with its status and how many of its cases measured nothing.
  assert.ok(sentence.includes("the gate ended refused (exit 1)"), sentence);
  assert.ok(sentence.includes("0 cases skipped"), sentence);
  assert.equal(detailText(report), report.detail);

  // After a failure the pipeline leaves the machine draining, and the sentence that withholds the button is that
  // fact — not the job, which has ended.
  assert.deepEqual(
    promoteOffered({
      role: "server",
      draining: true,
      running: false,
      candidate: candidateFacts({ ok: true, value: CANDIDATE }),
    }),
    { allowed: false, sentence: PROMOTE_DRAINING },
  );
  assert.ok(drainText(true)?.includes("draining"), "the drain has its own sentence");
});

// ---- ended, refused: the gate said no, and the machine was asked nothing ------------

test("a refused gate is one sentence with its detail, and the button comes back", () => {
  const detail =
    "the gate ran and the tree failed it (exit 1), so nothing was promoted: Nothing was asked of the machine.";
  const report = ended("refused", {
    gate: gate({ exit_code: 1, ending: "refused", skips: 2 }),
    detail,
  });
  const sentence = endingText(report);
  assert.ok(sentence !== null);
  assert.ok(sentence.includes("refused"), sentence);
  assert.ok(sentence.includes("2 cases skipped"), sentence);
  assert.equal(detailText(report), detail);

  // A refused gate left the machine as it found it, so nothing withholds a second press: the candidate is still
  // there and the operator may press again once the tree is fixed.
  assert.deepEqual(
    promoteOffered({
      role: "server",
      draining: false,
      running: false,
      candidate: candidateFacts({ ok: true, value: CANDIDATE }),
    }),
    { allowed: true, sentence: null },
  );
});

test("a tree the gate could not verify says so in its own words", () => {
  const report = ended("could-not-verify", {
    gate: gate({ exit_code: 3, ending: "stale crate set", skips: 0 }),
    detail: "the gate answered 3: the candidate's `Cargo.lock` names a crate set the running release does not vendor",
  });
  const sentence = endingText(report);
  assert.ok(sentence !== null);
  assert.ok(sentence.includes("could not verify"), sentence);
  assert.ok(sentence.includes("stale crate set"), sentence);
  // A signal has no exit status, and a state this build does not know is a word to print rather than a throw.
  assert.ok(gateText(gate({ exit_code: null, ending: "could not verify" }))?.includes("killed by a signal"));
  assert.equal(endingText(ended("something_new")), "the job ended as something_new");
});

test("a live candidate's ending says so, and carries no detail", () => {
  const report = ended("live", { gate: gate({ exit_code: 0, ending: "verified", skips: 0 }) });
  const sentence = endingText(report);
  assert.ok(sentence !== null);
  assert.ok(sentence.includes("live"), sentence);
  assert.ok(sentence.includes("the gate ended verified (exit 0)"), sentence);
  assert.equal(detailText(report), null, "a live candidate has no refusal to explain");
  assert.equal(detailText(ended("refused", { detail: "   " })), null, "whitespace is not a sentence");
});

// ---- the stale-report rule, and the body a press sends ------------------------------

test("a report written before the press is the previous run, not this one", () => {
  const accepted = 1_700_000_100;
  // The job's own `since` is later than the `202`'s, because the unit is queued before its pass begins.
  assert.equal(isThisRun(accepted + 5, accepted), true);
  assert.equal(isThisRun(accepted, accepted), true);
  assert.equal(isThisRun(accepted - 60, accepted), false, "an older report is the previous promotion");
  // A report the previous release's job wrote carries no `since` at all: this page cannot date it, so it is not this
  // run either.
  assert.equal(isThisRun(undefined, accepted), false);
  assert.equal(isThisRun(null, accepted), false);
});

test("a press sends the commit the page rendered", () => {
  // The route compares the body against `master`'s now, so a commit that landed while the page was open is a `422`
  // rather than a promotion of a tree nobody saw.
  assert.deepEqual(JSON.parse(promoteBody(CANDIDATE.commit)), { commit: CANDIDATE.commit });
  assert.ok(promoteBody(CANDIDATE.commit).includes(CANDIDATE.commit));
  assert.equal(
    candidateText({ found: true, candidate: CANDIDATE }).includes(CANDIDATE.commit),
    true,
    "the commit the body carries is not the commit the page drew",
  );
});

// ---- the screen, the paths and the card --------------------------------------------

test("the screen is a path in the app and two routes on the machine", () => {
  assert.equal(SERVICE_PATH, "/service");
  assert.equal(UPDATE_PATH, "/api/update");
  assert.equal(CANDIDATE_PATH, "/api/update/candidate");
  // One poll per few seconds: a gate prints a line every few seconds, and the report is a file.
  assert.equal(POLL_INTERVAL_MS, 3000);
  assert.ok(SERVICE_CARD_SENTENCE.includes("this machine"), SERVICE_CARD_SENTENCE);
});

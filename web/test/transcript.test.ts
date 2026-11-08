// The transcript's rules, folded over synthetic frames. There is no browser on the machine these
// gates run on, so these are the tests the session screen gets: the wire format HS-015's producer
// emits, the append-or-replace rule per message id, a tool call's own block and order, a message's
// usage, the turn boundary, the composer's affordance matrix and the 10k-delta budget (ADR-0018).
//
// Run with `npm test` in `web/`: node 24's own test runner, which strips the types. No dependency and
// no lock-file change (ADR-0017). This file sits outside `src`, so `tsc --noEmit` does not typecheck
// it — typing it would mean adding `@types/node` — but the gate *runs* it, and the extension in the
// `../src/transcript.ts` import is what node's ESM resolver requires.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  HISTORY_PAGE_LIMIT,
  NO_DETAIL,
  REASON_MAX,
  Transcript,
  affordances,
  budgetNotice,
  PROJECT_REFUSALS,
  createFailure,
  deletedNotice,
  FORCED_DELETE_LABEL,
  UNSAVED_WORK_NOTICE,
  historyPath,
  LOCAL_HOST,
  networkText,
  parseFrame,
  previewOffered,
  previewPath,
  previewUrl,
  parseTruncated,
  sessionIdFromPath,
  stateText,
  stoppedDetail,
  stoppedNotice,
  type Kept,
  streamPath,
  type Change,
  type Frame,
  type SessionState,
  CI_WARNING,
  COMMIT_LABEL,
  COMMIT_PROMPT,
  DIRTY_MERGE_REASON,
  NO_TARGET_REASON,
  RESOLVE_LABEL,
  RESOLVE_PROMPT,
  RUNNING_MERGE_REASON,
  ciWarning,
  conflictLine,
  countsLine,
  countsText,
  diffPath,
  fileLine,
  mergeBlocked,
  mergedNotice,
  preselectedTarget,
  publishQuestion,
  publishVisible,
  publishedNotice,
  sheetFailure,
  statusWord,
  targetOptions,
  type Changes,
} from "../src/transcript.ts";

// One delta, spelled the way the producer must spell it.
function delta(messageId: string, kind: "text" | "thinking", text: string): Frame {
  return { type: "delta", messageId, delta: kind, text };
}

// The single text a run of changes appended to one kind, so a test can assert the transcript's
// content without a DOM.
function appended(changes: Change[], messageId: string, kind: "text" | "thinking"): string {
  return changes
    .map((change) =>
      change.op === "delta" && change.messageId === messageId && change.delta === kind
        ? change.text
        : "",
    )
    .join("");
}

test("a delta frame is the pinned wire shape, and its kind names its output", () => {
  assert.deepEqual(
    parseFrame("message.delta", '{"message_id":"m1","kind":"text","text":"hi"}'),
    delta("m1", "text", "hi"),
  );
  assert.deepEqual(
    parseFrame("message.delta", '{"message_id":"m1","kind":"thinking","text":"hmm"}'),
    delta("m1", "thinking", "hmm"),
  );
});

test("a snapshot frame carries the whole message, and an absent channel is empty", () => {
  assert.deepEqual(
    parseFrame(
      "message.snapshot",
      '{"message_id":"m1","text":"done","thinking":"thought"}',
    ),
    { type: "snapshot", messageId: "m1", text: "done", thinking: "thought" },
  );
  // A text-only snapshot clears thinking that deltas had put on screen: replacement, not merge.
  assert.deepEqual(parseFrame("message.snapshot", '{"message_id":"m1","text":"done"}'), {
    type: "snapshot",
    messageId: "m1",
    text: "done",
    thinking: "",
  });
  // Neither kind named is an empty message, not a frame to drop: the producer said what the message
  // is, and the answer is "nothing" — which clears both nodes rather than leaving a stale half.
  assert.deepEqual(parseFrame("message.snapshot", '{"message_id":"m1"}'), {
    type: "snapshot",
    messageId: "m1",
    text: "",
    thinking: "",
  });
});

test("the turn boundary is its own frame and reads no payload", () => {
  assert.deepEqual(parseFrame("turn.start", "{}"), { type: "boundary", boundary: "start" });
  assert.deepEqual(parseFrame("turn.end", '{"reason":"settled"}'), {
    type: "boundary",
    boundary: "end",
  });
});

test("a frame this screen cannot render is dropped, not rendered blank", () => {
  for (const frame of [
    ["message.delta", "not json"],
    ["message.delta", "[]"],
    ["message.delta", "null"],
    ["message.delta", '"m1"'],
    ["message.delta", '{"kind":"text","text":"hi"}'],
    ["message.delta", '{"message_id":"","kind":"text","text":"hi"}'],
    ["message.delta", '{"message_id":"m1","kind":"tool","text":"hi"}'],
    ["message.delta", '{"message_id":"m1","kind":"text"}'],
    // A snapshot without an id names no block, which is the one field it cannot omit.
    ["message.snapshot", '{"text":"done"}'],
    // A tool call needs its own id, a `message.end` needs its usage, and a kind the page does not
    // render — a later story's — is ignored without a release, which is the point.
    ["tool.start", '{"message_id":"m1","name":"read_file"}'],
    ["tool.end", '{"tool_id":"","output":"x"}'],
    ["message.end", '{"message_id":"m1"}'],
    ["message.end", '{"message_id":"m1","usage":{"in":1}}'],
    ["message.end", '{"message_id":"m1","usage":[1,2]}'],
    ["usage", '{"in":1,"out":2}'],
    ["", ""],
  ] as const) {
    assert.equal(parseFrame(frame[0], frame[1]), null, `${frame[0]} ${frame[1]}`);
  }
});

test("a message's end carries its usage, keyed to the message it ends", () => {
  assert.deepEqual(
    parseFrame("message.end", '{"message_id":"m1","usage":{"in":1200,"out":340}}'),
    { type: "usage", messageId: "m1", input: 1200, output: 340 },
  );
  const transcript = new Transcript();
  transcript.apply(delta("m1", "text", "answered"));
  assert.deepEqual(
    transcript.apply({
      type: "usage",
      messageId: "m1",
      input: 1200,
      output: 340,
    }),
    [{ op: "usage", messageId: "m1", input: 1200, output: 340 }],
    "the usage names the message it belongs to and creates no block of its own",
  );
  assert.deepEqual(transcript.order(), ["m1"]);
  // A usage for a message never seen creates its block first, like a snapshot.
  const unseen = transcript.apply({ type: "usage", messageId: "m2", input: 1, output: 2 });
  assert.deepEqual(
    unseen.map((change) => change.op),
    ["message", "usage"],
  );
  assert.deepEqual(transcript.order(), ["m1", "m2"]);
});

test("a tool call is its own block, in its own order, never a message", () => {
  assert.deepEqual(
    parseFrame(
      "tool.start",
      '{"tool_id":"tool-1","name":"read_file","arguments":"src/main.ts"}',
    ),
    { type: "tool-start", toolId: "tool-1", name: "read_file", arguments: "src/main.ts" },
  );
  assert.deepEqual(
    parseFrame("tool.end", '{"tool_id":"tool-1","name":"read_file","output":"1  // The app"}'),
    {
      type: "tool-end",
      toolId: "tool-1",
      output: "1  // The app",
      // HS-033's three keys: a frame written without them (a durable log) parses with all three `null`.
      status: null,
      durationMs: null,
      outputRef: null,
      // HS-035's key: a frame written before it existed carries no pictures, which is the empty list.
      images: [],
    },
  );

  const transcript = new Transcript();
  const start = transcript.apply({
    type: "tool-start",
    toolId: "tool-1",
    name: "read_file",
    arguments: "src/main.ts",
  });
  assert.deepEqual(start, [
    { op: "tool", toolId: "tool-1" },
    { op: "tool-start", toolId: "tool-1", name: "read_file", arguments: "src/main.ts" },
  ]);
  // The second call interleaves with a message: two tool blocks keep their own order, and neither
  // is a message block.
  const second = transcript.apply({
    type: "tool-start",
    toolId: "tool-2",
    name: "grep",
    arguments: "turn",
  });
  assert.deepEqual(second.map((change) => change.op), ["tool", "tool-start"]);
  transcript.apply(delta("m1", "text", "read both"));
  const end = transcript.apply({
    type: "tool-end",
    toolId: "tool-1",
    output: "done",
    status: "ok",
    durationMs: 400,
    outputRef: "/api/sessions/0123456789abcdef/tool-output/tool-1",
    images: [],
  });
  assert.deepEqual(end, [
    {
      op: "tool-end",
      toolId: "tool-1",
      output: "done",
      status: "ok",
      durationMs: 400,
      outputRef: "/api/sessions/0123456789abcdef/tool-output/tool-1",
      images: [],
    },
  ]);
  assert.deepEqual(transcript.toolOrder(), ["tool-1", "tool-2"]);
  assert.deepEqual(transcript.order(), ["m1"], "a tool call is never a message block");
});

test("a tool.end without its tool.start creates the block first", () => {
  const transcript = new Transcript();
  const changes = transcript.apply({
    type: "tool-end",
    toolId: "tool-9",
    output: "the output",
    status: null,
    durationMs: null,
    outputRef: null,
    images: [],
  });
  assert.deepEqual(changes, [
    { op: "tool", toolId: "tool-9" },
    {
      op: "tool-end",
      toolId: "tool-9",
      output: "the output",
      status: null,
      durationMs: null,
      outputRef: null,
      images: [],
    },
  ]);
  assert.deepEqual(transcript.toolOrder(), ["tool-9"]);
  assert.deepEqual(transcript.order(), []);
});

test("two messages interleaved keep one block each, thinking apart from text", () => {
  const transcript = new Transcript();
  const first = transcript.apply(delta("a", "thinking", "let me "));
  const second = transcript.apply(delta("b", "text", "hello "));
  const third = transcript.apply(delta("a", "text", "look"));
  const fourth = transcript.apply(delta("b", "thinking", "hm"));
  assert.deepEqual(transcript.order(), ["a", "b"]);
  // The block is created once, before its first delta, and only for its own id.
  assert.deepEqual(first, [
    { op: "message", messageId: "a" },
    { op: "delta", messageId: "a", delta: "thinking", text: "let me " },
  ]);
  assert.deepEqual(second[0], { op: "message", messageId: "b" });
  assert.deepEqual(
    third,
    [{ op: "delta", messageId: "a", delta: "text", text: "look" }],
    "a seen id creates no second block",
  );
  assert.deepEqual(fourth, [{ op: "delta", messageId: "b", delta: "thinking", text: "hm" }]);
  assert.equal(appended([first[1], third[0]], "a", "text"), "look");
  assert.equal(appended([second[1], fourth[0]], "b", "thinking"), "hm");
});

test("a snapshot replaces by message id, safe after partial deltas", () => {
  const transcript = new Transcript();
  transcript.apply(delta("m1", "text", "half"));
  transcript.apply(delta("m1", "thinking", "thought"));
  const changes = transcript.apply({
    type: "snapshot",
    messageId: "m1",
    text: "the whole message",
    thinking: "",
  });
  assert.deepEqual(
    changes,
    [
      {
        op: "snapshot",
        messageId: "m1",
        text: "the whole message",
        thinking: "",
      },
    ],
    "one replacement, and no new block",
  );
  assert.deepEqual(transcript.order(), ["m1"]);
  // A snapshot of a message never seen creates the block first, then replaces it.
  const unseen = transcript.apply({
    type: "snapshot",
    messageId: "m2",
    text: "imported",
    thinking: "why",
  });
  assert.deepEqual(
    unseen.map((change) => change.op),
    ["message", "snapshot"],
  );
  assert.deepEqual(transcript.order(), ["m1", "m2"]);
});

test("a turn boundary is in order, and turn.start numbers the turn", () => {
  const transcript = new Transcript();
  const start = transcript.apply({ type: "boundary", boundary: "start" });
  transcript.apply(delta("m1", "text", "answer"));
  const end = transcript.apply({ type: "boundary", boundary: "end" });
  const second = transcript.apply({ type: "boundary", boundary: "start" });
  assert.deepEqual(start, [{ op: "boundary", boundary: "start", turn: 1 }]);
  assert.deepEqual(end, [{ op: "boundary", boundary: "end", turn: 1 }]);
  assert.deepEqual(second, [{ op: "boundary", boundary: "start", turn: 2 }]);
  assert.deepEqual(transcript.order(), ["m1"]);
});

test("a delta with no text changes nothing, but its block still exists", () => {
  const transcript = new Transcript();
  const changes = transcript.apply(delta("m1", "text", ""));
  assert.deepEqual(changes, [{ op: "message", messageId: "m1" }]);
  assert.deepEqual(transcript.order(), ["m1"]);
});

test("Steer and Abort exist only while a turn runs", () => {
  const none = { prompt: false, steer: false, abort: false, resume: false };
  assert.deepEqual(affordances("running"), { ...none, steer: true, abort: true });
  assert.deepEqual(affordances("idle"), { ...none, prompt: true });
  assert.deepEqual(affordances("starting"), none);
  assert.deepEqual(affordances("deleting"), none);
});

test("Resume exists in exactly the two states the transition table resumes from", () => {
  // HS-018: the table has two `resume` rows — `stopped` and `error` — so those are the two states whose
  // one action is Resume, and no other state offers it.
  for (const state of ["stopped", "error"] as SessionState[]) {
    assert.deepEqual(
      affordances(state),
      { prompt: false, steer: false, abort: false, resume: true },
      state,
    );
  }
  for (const state of ["starting", "idle", "running", "deleting"] as SessionState[]) {
    assert.equal(affordances(state).resume, false, state);
  }
});

test("the stream is the ADR-0014 path and no resume parameter is invented", () => {
  assert.equal(streamPath("0123456789abcdef"), "/api/sessions/0123456789abcdef/events");
  assert.ok(!streamPath("abc").includes("since="), "the browser sends Last-Event-ID, not the page");
});

test("an evicted session's row says nothing was deleted", () => {
  // HS-020's reason word, and spec item 9's promise: the cap stops a session, it never removes one.
  const notice = stoppedNotice("evicted");
  assert.ok(notice !== null, "an evicted row said nothing");
  assert.match(notice, /nothing was deleted/);
  assert.match(notice, /resume/);
});

test("a session stopped for sitting idle says so, and says nothing was deleted", () => {
  // HS-021's reason word. The cap's sentence is wrong here — an idle stop did not run out of room —
  // but the promise is the same one, and spec item 9's "nothing is deleted" covers this stop too:
  // the worktree, the transcript and the row are all still there, and a resume continues the session.
  const notice = stoppedNotice("idle_stop");
  assert.ok(notice !== null, "an idle-stopped row said nothing");
  assert.match(notice, /nothing was deleted/);
  assert.match(notice, /resume/);
  assert.doesNotMatch(notice, /make room/, "an idle stop is not the cap's sentence");
  assert.notEqual(notice, stoppedNotice("evicted"), "two reasons, one sentence");
});

test("every other reason is the caller's to show, so no sentence is invented for it", () => {
  // The map is two words wide: a forced abort is not data loss and a delete's own reason must not be
  // softened, so both get `null` and the badge's `state · reason` stands alone.
  for (const reason of [
    null,
    "",
    "forced_abort",
    "deleted",
    "turn_timeout",
    "server_restart",
    // HS-030's clean exit: `stopped · pi_exit` is the whole message, and the badge already carries it.
    "pi_exit",
  ]) {
    assert.equal(stoppedNotice(reason), null, String(reason));
  }
});

test("a badge shows a one-word reason as it is", () => {
  // The four reason words the machine writes on its own, and the two a client request does. Nothing about a
  // single-line reason is the renderer's to change.
  for (const reason of ["evicted", "idle_stop", "forced_abort", "deleted", "turn_timeout", "pi_exit"]) {
    assert.equal(stateText("stopped", reason), `stopped · ${reason}`);
  }
  assert.equal(stateText("idle", null), "idle", "a reasonless row is its state alone");
});

test("interrupted is a reason on an idle row, not a state and not a notice", () => {
  // HS-044: a `kill -9` mid-turn comes back as an `idle` row whose reason says what happened to the turn.
  // The badge is the only place the operator learns it — so it has to render, and it must not borrow a
  // sentence from the two reasons that promise nothing was lost (`stoppedNotice` is for stopped rows, and
  // an interrupted turn is not a loss to explain but a fact to state).
  assert.equal(stateText("idle", "interrupted"), "idle · interrupted");
  assert.equal(stoppedNotice("interrupted"), null);
  for (const state of ["starting", "idle", "running", "stopped", "error", "deleting"] as const) {
    assert.notEqual(state, "interrupted" as never, "interrupted became a state");
  }
});

test("a badge shows a multi-line reason's last line, clipped", () => {
  // HS-030's unexpected exit is the first reason that is more than a sentence: the last few lines the
  // container printed, one per line. The badge has room for one of them, and the useful one is the last —
  // the error itself rather than the oldest line of the tail. `session.log` keeps all of it.
  const tail = "error: no turn will run\nstack: frame 3\nstack: frame 0";
  assert.equal(stateText("error", tail), "error · stack: frame 0");

  const long = "".padEnd(REASON_MAX + 20, "x");
  const clipped = stateText("error", long);
  assert.equal(clipped, `error · ${"x".repeat(REASON_MAX)}…`);
  assert.ok(clipped.length < long.length, "the badge clipped nothing");

  // Blank lines are not lines: a tail padded with them still shows its last real one.
  assert.equal(stateText("error", "boom\n\n  \n"), "error · boom");
});

test("the detail line says the same thing whether the row was drawn or turned", () => {
  // HS-020's lesson from the review: the span is written twice — by `sessionRow` when the list is
  // drawn, and by `applyStatus` when a `session.status` frame turns a live row into a stopped one,
  // which is how an eviction arrives while the operator watches. One function, so they cannot differ.
  assert.equal(stoppedDetail("stopped", "evicted"), stoppedNotice("evicted"));
  assert.notEqual(stoppedDetail("stopped", "evicted"), NO_DETAIL);
  assert.equal(stoppedDetail("stopped", "idle_stop"), stoppedNotice("idle_stop"));
  assert.notEqual(stoppedDetail("stopped", "idle_stop"), NO_DETAIL);

  // A stopped row whose reason earns no sentence keeps the dash, and so does every other state: the
  // span is only visible on a stopped row anyway, and a stale sentence under `running` would be a
  // claim about a session that is working.
  assert.equal(stoppedDetail("stopped", null), NO_DETAIL);
  assert.equal(stoppedDetail("stopped", "forced_abort"), NO_DETAIL);
  for (const state of ["starting", "idle", "running", "error", "deleting"] as SessionState[]) {
    assert.equal(stoppedDetail(state, "evicted"), NO_DETAIL, state);
  }
});

test("the screen reads its id from /sessions/<id> and from nowhere else", () => {
  assert.equal(sessionIdFromPath("/sessions/0123456789abcdef"), "0123456789abcdef");
  for (const path of ["/", "/sessions", "/sessions/", "/sessions/a/b", "/api/sessions/abc", ""]) {
    assert.equal(sessionIdFromPath(path), null, path);
  }
});

test("a steer is one block per frame, drawn once, and it is not a message", () => {
  // HS-017's frame: `turn.steer` with the operator's guidance. It carries no message id because it
  // has nothing a later frame would update, so the model does no bookkeeping for it at all.
  const frame = parseFrame("turn.steer", JSON.stringify({ text: "and the lexer too" }));
  assert.deepEqual(frame, { type: "steer", text: "and the lexer too" });

  const transcript = new Transcript();
  const first = transcript.apply(frame as Frame);
  assert.deepEqual(first, [{ op: "steer", text: "and the lexer too" }]);
  assert.deepEqual(transcript.apply(delta("m1", "text", "still working")), [
    { op: "message", messageId: "m1" },
    { op: "delta", messageId: "m1", delta: "text", text: "still working" },
  ]);
  // A second steer is a second block, and three steers are three: no id means no folding.
  transcript.apply({ type: "steer", text: "and the tests" });
  assert.deepEqual(transcript.apply({ type: "steer", text: "and the commit" }), [
    { op: "steer", text: "and the commit" },
  ]);
  // Steers are not messages, so the message order is untouched by them.
  assert.deepEqual(transcript.order(), ["m1"]);
  assert.deepEqual(transcript.toolOrder(), []);
});

test("a steer with no text is dropped, and a non-string text with it", () => {
  for (const payload of [JSON.stringify({}), JSON.stringify({ text: 7 }), JSON.stringify({ text: null })]) {
    assert.equal(parseFrame("turn.steer", payload), null, payload);
  }
  assert.equal(parseFrame("turn.steer", "not json"), null);
  assert.equal(parseFrame("turn.steer", "[]"), null);
});

test("a cancelled extension dialog is one block per frame, drawn once, and it is not a message", () => {
  // HS-035's frame: `turn.error` with the reason and the method an extension asked for. It has no message id
  // for a steer's reason — one frame, one block, nothing a later frame would update — so the model keeps no
  // bookkeeping for it and the block lands where in the turn it happened.
  const frame = parseFrame(
    "turn.error",
    JSON.stringify({
      reason: "an extension asked for select, and a session cannot answer a dialog, so it was cancelled",
      method: "select",
    }),
  );
  assert.deepEqual(frame, {
    type: "error",
    reason: "an extension asked for select, and a session cannot answer a dialog, so it was cancelled",
    method: "select",
  });

  const transcript = new Transcript();
  assert.deepEqual(transcript.apply(frame as Frame), [
    {
      op: "error",
      reason: "an extension asked for select, and a session cannot answer a dialog, so it was cancelled",
      method: "select",
    },
  ]);
  // It creates no message block and no tool block: a frame about what the turn could not do is not part of
  // either, and a turn that read one has no message to fold it into.
  assert.deepEqual(transcript.order(), []);
  assert.deepEqual(transcript.toolOrder(), []);
  // Two dialogs in one turn are two blocks, and the second does not overwrite the first.
  const second = {
    type: "error",
    reason: "an extension asked for confirm, and a session cannot answer a dialog, so it was cancelled",
    method: "confirm",
  } as const;
  assert.deepEqual(transcript.apply(second), [
    { op: "error", reason: second.reason, method: "confirm" },
  ]);
});

test("a turn error with no reason is dropped, and one that is not an object with it", () => {
  // The reason is the whole point of the frame: a block with a method and no sentence would be a heading with
  // nothing under it. A missing method is the empty string instead — the sentence carries the meaning.
  for (const payload of [
    JSON.stringify({}),
    JSON.stringify({ reason: "" }),
    JSON.stringify({ reason: 7 }),
    JSON.stringify({ reason: null, method: "select" }),
  ]) {
    assert.equal(parseFrame("turn.error", payload), null, payload);
  }
  assert.equal(parseFrame("turn.error", "not json"), null);
  assert.equal(parseFrame("turn.error", "[]"), null);
  assert.deepEqual(parseFrame("turn.error", JSON.stringify({ reason: "why" })), {
    type: "error",
    reason: "why",
    method: "",
  });
});

test("10k deltas are 10k appends into one block, never a re-render", () => {
  const transcript = new Transcript();
  const piece = "0123456789";
  const total = 10_000;
  let appends = 0;
  let replacements = 0;
  let createdAt = 0;
  let written = "";
  for (let index = 0; index < total; index += 1) {
    const changes = transcript.apply(delta("m1", "text", piece));
    for (const change of changes) {
      if (change.op === "message") {
        createdAt += 1;
      } else if (change.op === "delta") {
        appends += 1;
        written += change.text;
      } else {
        replacements += 1;
      }
    }
  }
  assert.equal(createdAt, 1, "one message id is one block");
  assert.equal(appends, total, "one delta is one append");
  assert.equal(replacements, 0, "a delta never replaces what is already on screen");
  assert.equal(written, piece.repeat(total), "the appends reconstruct the message exactly");
  assert.deepEqual(transcript.order(), ["m1"]);
});

test("the budget's sentence needs a settled turn and no reported usage", () => {
  // Spec item 13's contingency, as a rule (HS-023). Four cases, and the two that must stay silent are
  // the two a row's totals cannot tell apart: "no turn has run yet" and "the agent did report usage".
  assert.equal(
    budgetNotice({ settled: 0, usage: false }),
    null,
    "a session no turn has run in says nothing about usage",
  );
  assert.equal(
    budgetNotice({ settled: 1, usage: true }),
    null,
    "a session whose usage was reported says nothing",
  );
  assert.equal(budgetNotice({ settled: 2, usage: true }), null);
  assert.equal(
    budgetNotice({ settled: 1, usage: false }),
    "no usage reported — the token budget has nothing to count",
    "a settled turn that reported no usage is what the screen says so about",
  );
});

test("the budget's sentence is not one of the stopped row's sentences", () => {
  // Two vocabularies, one screen: `stoppedNotice` reassures about work that was preserved, and this
  // one is about a guard that cannot count. Sharing a word would conflate the two policies.
  const budget = budgetNotice({ settled: 1, usage: false });
  assert.notEqual(budget, stoppedNotice("evicted"));
  assert.notEqual(budget, stoppedNotice("idle_stop"));
  assert.notEqual(budget, stoppedNotice("forced_abort"));
});

test("the model counts settled turns and notices a usage frame", () => {
  // The two facts `budgetNotice` reads, folded from the stream: a `turn.start` is not a settle, a
  // `message.end` with counts is what "usage was reported" means, and a `message.end` without them
  // never becomes a frame at all.
  const transcript = new Transcript();
  assert.deepEqual(transcript.spend(), { settled: 0, usage: false });

  transcript.apply({ type: "boundary", boundary: "start" });
  assert.deepEqual(transcript.spend(), { settled: 0, usage: false }, "a turn that started has not settled");

  transcript.apply({ type: "usage", messageId: "m1", input: 1200, output: 340 });
  transcript.apply({ type: "boundary", boundary: "end" });
  assert.deepEqual(transcript.spend(), { settled: 1, usage: true });
  assert.equal(budgetNotice(transcript.spend()), null, "a turn that reported usage says nothing");

  const quiet = new Transcript();
  quiet.apply({ type: "boundary", boundary: "start" });
  quiet.apply({ type: "boundary", boundary: "end" });
  assert.deepEqual(quiet.spend(), { settled: 1, usage: false });
  assert.equal(budgetNotice(quiet.spend()), "no usage reported — the token budget has nothing to count");
});

test("a message.end with no usage is dropped, so it cannot claim a spend of zero", () => {
  // The parse rule the writer's tally and the screen share: no `usage` object is no frame, not a
  // frame of zeros. A screen that rendered `in 0 · out 0` would say the message was free.
  assert.equal(parseFrame("message.end", '{"message_id":"m1"}'), null);
  assert.equal(parseFrame("message.end", '{"message_id":"m1","usage":{}}'), null);
  assert.equal(parseFrame("message.end", '{"message_id":"m1","usage":{"in":"1","out":2}}'), null);
  // A count is a non-negative integer, the same rule the writer's tally applies in Rust (HS-023): a
  // negative or fractional cost is not a cost, and rendering one would show a number the budget never
  // added.
  assert.equal(parseFrame("message.end", '{"message_id":"m1","usage":{"in":-5,"out":340}}'), null);
  assert.equal(parseFrame("message.end", '{"message_id":"m1","usage":{"in":1.5,"out":340}}'), null);
  assert.deepEqual(parseFrame("message.end", '{"message_id":"m1","usage":{"in":1200,"out":340}}'), {
    type: "usage",
    messageId: "m1",
    input: 1200,
    output: 340,
  });
});

test("a project's refusal is its sentence alone, and every other answer keeps its code", () => {
  // HS-026: the three refusals are about the project, and the story asks the page to explain them in
  // one sentence rather than to show a code the phone cannot act on. The codes are the contract and
  // the page still refuses to hide a *different* failure behind a sentence alone: a client that cannot
  // tell `bad_project` from silence would not know the request never reached a repository.
  const sentence = "the project's .git is not a directory, so it was refused";
  for (const code of ["git_not_directory", "submodules_present", "worktree_config_enabled"]) {
    assert.equal(createFailure(code, sentence), sentence, `${code} is a project refusal`);
  }
  assert.equal(createFailure("bad_project", sentence), `bad_project: ${sentence}`);
  assert.equal(createFailure("cap_reached", sentence), `cap_reached: ${sentence}`);
  assert.equal(createFailure("unreachable", "Request timed out"), "unreachable: Request timed out");
});

test("the three project refusals are three codes, not one code and three sentences", () => {
  // The story's demo asks for a distinct code per hole, so the page's list is the check that the three
  // stay three: folding them into one code would make the list below a list of one.
  assert.deepEqual(
    [...PROJECT_REFUSALS].sort(),
    ["git_not_directory", "submodules_present", "worktree_config_enabled"],
  );
});

test("a coalesced delta renders exactly like the deltas it replaced", () => {
  // HS-059 merges the deltas of one message and kind inside one commit into one row. The page must not be
  // able to tell: a delta is one appended text node, so one frame of the whole sentence and three frames of
  // its parts are the same block, the same order and the same text (ADR-0018).
  const many = new Transcript();
  const split = ["The shell is ", "the session screen ", "and the list."].flatMap((text) =>
    many.apply(delta("m1", "text", text)),
  );
  const one = new Transcript();
  const merged = one.apply(delta("m1", "text", "The shell is the session screen and the list."));

  assert.equal(appended(split, "m1", "text"), appended(merged, "m1", "text"));
  assert.deepEqual(many.order(), one.order(), "one message is one block either way");
  assert.equal(many.order().length, 1, "three deltas made more than one block");
});

test("the writer's merged payload is a frame the page parses", () => {
  // The seam between the two halves of "the transcript is identical": the coalescer re-serialises a merged
  // row's payload (`events::Delta`, field order `message_id`, `kind`, `text`), and this is that exact byte
  // shape going through the renderer's own parser. Without it, each half could be right and the pair wrong.
  const frame = parseFrame("message.delta", '{"message_id":"m1","kind":"text","text":"a b"}');
  assert.notEqual(frame, null, "the coalescer's payload is not a delta frame");
  const transcript = new Transcript();
  const changes = transcript.apply(frame as Frame);
  assert.equal(appended(changes, "m1", "text"), "a b");
  // And a thinking frame lands in its own node, never in the text one — the merge must not blur the two.
  const thinking = transcript.apply(delta("m1", "thinking", "hmm"));
  assert.equal(appended(thinking, "m1", "text"), "", "a thinking delta became text");
  assert.equal(appended(thinking, "m1", "thinking"), "hmm");
});

// A turn boundary's frame, which is what a turn number is derived from.
function boundary(kind: "start" | "end"): Frame {
  return { type: "boundary", boundary: kind };
}

// The turn number a boundary change names.
function turnOf(changes: Change[]): number {
  const change = changes[0];
  assert.ok(change !== undefined && change.op === "boundary", "the change is not a boundary");
  return change.turn;
}

test("a windowed transcript numbers its turns like a whole-log one", () => {
  // HS-061: the page numbers a turn from the `turn.start` frames it has seen. A replay that starts at the
  // 50th most recent message has seen none of the turns before it, so without the frame's `turns_before`
  // the first turn on screen would be labelled "turn 1" while the log says turn 40.
  const whole = new Transcript();
  const wholeTurns = [1, 2, 3, 4].map(() => turnOf(whole.apply(boundary("start"))));

  const windowed = new Transcript();
  windowed.setTurnBase(3);
  const windowedTurns = [1, 2].map(() => turnOf(windowed.apply(boundary("start"))));

  assert.deepEqual(wholeTurns, [1, 2, 3, 4]);
  assert.deepEqual(
    windowedTurns,
    [4, 5],
    "a truncated replay renumbered its turns from one",
  );
});

test("a truncation frame names a cursor and how many turns it skipped", () => {
  assert.deepEqual(parseTruncated('{"before_seq":41,"turns_before":3}'), {
    beforeSeq: 41,
    turnsBefore: 3,
  });
  // A frame with no usable cursor has nothing to offer a page request, so it is dropped.
  for (const data of ["", "{}", '{"before_seq":0}', '{"before_seq":"41"}', "not json", "[]"]) {
    assert.equal(parseTruncated(data), null, `${data} was accepted as a cursor`);
  }
  // A missing turn count is 0, not a refusal: it is wrong about labels, not about content.
  assert.deepEqual(parseTruncated('{"before_seq":7}'), { beforeSeq: 7, turnsBefore: 0 });
});

test("a history path carries the cursor and the page size", () => {
  assert.equal(
    historyPath("0123456789abcdef", 41),
    `/api/sessions/0123456789abcdef/events/history?before=41&limit=${HISTORY_PAGE_LIMIT}`,
  );
  // The page size the button sends is the server's own default, so the two cannot drift.
  assert.equal(HISTORY_PAGE_LIMIT, 200);
});

test("a page folded at its own turn base continues the live transcript's numbering", () => {
  // The two halves of "load earlier" meeting: the live model has already numbered its turns from the
  // truncation frame, and the page that is prepended to it must end where the live transcript begins.
  const live = new Transcript();
  live.setTurnBase(5);
  assert.equal(turnOf(live.apply(boundary("start"))), 6);

  const page = new Transcript();
  page.setTurnBase(3);
  const pageTurns = [1, 2].map(() => turnOf(page.apply(boundary("start"))));
  assert.deepEqual(pageTurns, [4, 5], "the page did not end where the live transcript began");
});

test("a tool.end carries the verdict, the duration and the output's own address", () => {
  // HS-033's three keys, and the two ways a frame can arrive without them: a row written before they
  // existed (a durable log) and a producer whose store refused the file.
  assert.deepEqual(
    parseFrame(
      "tool.end",
      '{"tool_id":"tool-1","name":"bash","output":"hello\\n","status":"error",' +
        '"duration_ms":1234,"output_ref":"/api/sessions/0123456789abcdef/tool-output/tool-1"}',
    ),
    {
      type: "tool-end",
      toolId: "tool-1",
      output: "hello\n",
      status: "error",
      durationMs: 1234,
      outputRef: "/api/sessions/0123456789abcdef/tool-output/tool-1",
      images: [],
    },
  );
  // A duration that is not a whole number of milliseconds is not a duration: both refusals are the same
  // rule the usage counts follow (`count`), so a `1.5` renders as "the log did not say" rather than "1.5s".
  const fractional = parseFrame(
    "tool.end",
    '{"tool_id":"tool-1","output":"x","status":"ok","duration_ms":1.5}',
  );
  assert.deepEqual(fractional, {
    type: "tool-end",
    toolId: "tool-1",
    output: "x",
    status: "ok",
    durationMs: null,
    outputRef: null,
    images: [],
  });
  // The change the DOM half applies, in one object: no second frame can update a tool block.
  const transcript = new Transcript();
  transcript.apply({
    type: "tool-start",
    toolId: "tool-1",
    name: "bash",
    arguments: "ls",
  });
  const [change] = transcript.apply({
    type: "tool-end",
    toolId: "tool-1",
    output: "preview",
    status: "ok",
    durationMs: 20,
    outputRef: "/api/sessions/0123456789abcdef/tool-output/tool-1",
    images: [],
  });
  assert.deepEqual(change, {
    op: "tool-end",
    toolId: "tool-1",
    output: "preview",
    status: "ok",
    durationMs: 20,
    outputRef: "/api/sessions/0123456789abcdef/tool-output/tool-1",
    images: [],
  });
});

test("a message.end that reports nothing is the same as one that reports no usage", () => {
  // ADR-0042's rule, on the client's side of the wire: the producer omits `usage` for a zero sum, so this
  // parse is what keeps a *durable* row written before the rule from hiding "no usage reported".
  assert.equal(
    parseFrame("message.end", '{"message_id":"m1","usage":{"in":0,"out":0}}'),
    null,
  );
  // A prompt served entirely from cache is a real cost, and one count of zero is not silence.
  assert.deepEqual(parseFrame("message.end", '{"message_id":"m1","usage":{"in":512,"out":0}}'), {
    type: "usage",
    messageId: "m1",
    input: 512,
    output: 0,
  });
  // And the sentence the screen draws for it: a turn settled and nothing was reported.
  assert.equal(budgetNotice({ settled: 1, usage: false }), "no usage reported — the token budget has nothing to count");
});

test("a tool.end names the pictures its result carried, and one it cannot link is dropped", () => {
  // HS-035: the frame carries the block's index, pi's mime and the path that serves the bytes — never a byte of
  // the picture, for the reason a tool's output is a file (the log is replayed on every reconnect).
  const payload = JSON.stringify({
    tool_id: "call_1",
    name: "read",
    output: "Read image file [image/png][image]",
    status: "ok",
    duration_ms: 73,
    output_ref: "/api/sessions/s/tool-output/call_1",
    images: [
      { index: 1, mime: "image/png", output_ref: "/api/sessions/s/tool-image/call_1/1" },
      // No path: there is nothing to link, so it is dropped rather than drawn as a dead anchor.
      { index: 2, mime: "image/png" },
      // Not an object at all.
      "image",
      // No index: the pair the frame is keyed by is incomplete.
      { mime: "image/png", output_ref: "/api/sessions/s/tool-image/call_1/3" },
    ],
  });
  const frame = parseFrame("tool.end", payload);
  assert.deepEqual(frame, {
    type: "tool-end",
    toolId: "call_1",
    output: "Read image file [image/png][image]",
    status: "ok",
    durationMs: 73,
    outputRef: "/api/sessions/s/tool-output/call_1",
    images: [{ index: 1, mime: "image/png", outputRef: "/api/sessions/s/tool-image/call_1/1" }],
  });
  // The transcript hands the list to the page unchanged: a picture is nothing the model folds or counts.
  const transcript = new Transcript();
  transcript.apply({ type: "tool-start", toolId: "call_1", name: "read", arguments: "{}" });
  const changes = transcript.apply(frame as Frame);
  assert.deepEqual(changes, [
    {
      op: "tool-end",
      toolId: "call_1",
      output: "Read image file [image/png][image]",
      status: "ok",
      durationMs: 73,
      outputRef: "/api/sessions/s/tool-output/call_1",
      images: [{ index: 1, mime: "image/png", outputRef: "/api/sessions/s/tool-image/call_1/1" }],
    },
  ]);
  // And `images` being something other than a list is the empty list, not a crash: a frame is text off a wire.
  for (const wrong of [JSON.stringify({ tool_id: "call_1", images: {} }), JSON.stringify({ tool_id: "call_1", images: 7 })]) {
    const parsed = parseFrame("tool.end", wrong);
    assert.deepEqual((parsed as { images: unknown }).images, [], wrong);
  }
});

test("the Internet-access phrase is two words, and neither promises a route", () => {
  // HS-038: spec item 2's checkbox is **topology**, so the page must not describe it as a setting with a direct
  // route behind it. The assertions are about what each case *claims*, because a phrase that read the same either
  // way would make the checkbox look inert — which is the bug the story is about.
  assert.equal(networkText(true), "internet on");
  assert.equal(networkText(false), "internet off");
  assert.notEqual(networkText(true), networkText(false));
  for (const phrase of [networkText(true), networkText(false)]) {
    // The feature's name is the checkbox's label, not the state's: "internet access" would not say which way it
    // is set, and it is not what the session screen's own span has ever said (HS-013's markup).
    assert.doesNotMatch(phrase, /access/i, phrase);
    // Nor a configuration: the environment is identical in both cases (ADR-0045), so naming one would be a lie
    // about the difference and would send an operator looking for something to change.
    assert.doesNotMatch(phrase, /HTTPS_PROXY|http_proxy|environment variable/i, phrase);
  }
});

// ---- the preview link (HS-041) --------------------------------------------------------------

// The session screen's link to what the agent built is the preview listener's own origin plus the path the
// listener parses. Two rules, because either half can be wrong on its own: the host segment (that is the
// listener's, not the page's) and which states offer a link at all.
test("a preview path spells the host segment the listener parses", () => {
  assert.equal(previewPath("0123456789abcdef"), "/local/0123456789abcdef/");
  assert.equal(LOCAL_HOST, "local");
  assert.equal(previewPath("0123456789abcdef").startsWith(`/${LOCAL_HOST}/`), true);
});

test("a preview URL is the origin and the path, with one slash between them", () => {
  const id = "0123456789abcdef";
  for (const origin of ["https://machine.tailnet.ts.net:8444", "https://machine.tailnet.ts.net:8444/"]) {
    assert.equal(
      previewUrl(origin, id),
      `https://machine.tailnet.ts.net:8444/local/${id}/`,
      origin,
    );
  }
  assert.equal(previewUrl("http://localhost:8081", id), `http://localhost:8081/local/${id}/`);
});

test("a preview is offered exactly by the states that have a container", () => {
  const offered: SessionState[] = ["starting", "running", "idle"];
  const hidden: SessionState[] = ["stopped", "error", "deleting"];
  for (const state of offered) {
    assert.equal(previewOffered(state), true, state);
  }
  for (const state of hidden) {
    assert.equal(previewOffered(state), false, state);
  }
});

// ---- what a delete keeps (HS-043, spec item 9) ------------------------------------------------

// The session screen's sentence after a delete was accepted. The command comes from the answer, so these
// cases are about the three shapes the server can send and never about the command's spelling: a second
// spelling in the page is how the copy an operator taps stops being the command that works.
function kept(branch: string | null, wip: string | null): Kept {
  return {
    branch,
    wip,
    recovery: wip === null ? null : `git branch rescue ${wip}`,
  };
}

test("a delete that kept nothing says so", () => {
  assert.equal(deletedNotice(kept(null, null)), "deleted — nothing was kept");
});

test("a delete says an answer it could not get is not an empty one", () => {
  // `null` is the repository refusing to answer (a base commit a row does not have, a repository git cannot
  // read), and the page may not turn that into "nothing was kept" — a claim the server declined to make.
  const answer = deletedNotice(null);
  assert.equal(answer, "deleted");
  assert.doesNotMatch(answer, /nothing was kept/i, answer);
});

test("a kept branch is named, and a kept ref brings the command that recovers it", () => {
  const branch = "agent/0123456789abcdef";
  const wip = "refs/holosmith/wip/0123456789abcdef";
  const both = deletedNotice(kept(branch, wip));
  assert.match(both, /deleted/, both);
  assert.match(both, new RegExp(branch), both);
  assert.match(both, new RegExp(wip), both);
  assert.match(both, new RegExp(`git branch rescue ${wip}`), both);
  // The command is the answer's, character for character: the page adds no prefix and no punctuation.
  assert.equal(both.includes(kept(branch, wip).recovery as string), true, both);
});

test("a delete that kept only the ref still names the command, and one that kept only a branch does not", () => {
  const wip = "refs/holosmith/wip/0123456789abcdef";
  const refOnly = deletedNotice(kept(null, wip));
  assert.match(refOnly, new RegExp(`git branch rescue ${wip}`), refOnly);

  const branchOnly = deletedNotice(kept("agent/0123456789abcdef", null));
  assert.equal(branchOnly.includes("git branch rescue"), false, branchOnly);
  assert.equal(branchOnly.includes("nothing was kept"), false, branchOnly);
});

test("a delete's sentence is not a stop's", () => {
  // `stoppedNotice` promises "nothing was deleted", which a stop may and a delete may not. The two families
  // share no sentence, so a delete that kept something can never read like a reassurance.
  const deleteSentence = deletedNotice(kept("agent/0123456789abcdef", null));
  for (const reason of ["evicted", "idle_stop"]) {
    const stopSentence = stoppedNotice(reason) as string;
    assert.notEqual(deleteSentence, stopSentence, reason);
    assert.equal(deleteSentence.includes(stopSentence), false, reason);
  }
});

test("the refused-snapshot sentence offers a decision, not a query string", () => {
  // The envelope's own message names `?force=true`; on a phone the screen shows a button instead, so the
  // sentence the page puts beside it must not send the operator looking for a URL to edit.
  assert.match(UNSAVED_WORK_NOTICE, /could not be saved/, UNSAVED_WORK_NOTICE);
  assert.doesNotMatch(UNSAVED_WORK_NOTICE, /force|\?/, UNSAVED_WORK_NOTICE);
  assert.equal(FORCED_DELETE_LABEL, "Delete anyway");
});

// --- the Changes sheet's rules (HS-058) ----------------------------------------------------------
//
// The sheet is the first screen since phase 1 with rules of its own, and `npm test` is the only gate that can see
// them: no `case_*` may need a browser. Every assertion below is a sentence or a branch the screen shows.

// One changes answer, with the two shapes the tests below vary. Written as a function rather than a constant so a
// test that changes one field cannot be read by the next test.
function changes(overrides: Partial<Changes> = {}): Changes {
  return {
    branch: "agent/0123456789abcdef",
    base_commit: "0".repeat(40),
    head: "1".repeat(40),
    target: "main",
    ahead: 2,
    behind: 1,
    dirty: false,
    mergeable: true,
    mergeable_reason: null,
    files: [],
    touches_ci: false,
    choices: { targets: ["main"], remotes: [] },
    ...overrides,
  };
}

test("a file row names the path, the status as a word and both counts", () => {
  assert.equal(
    fileLine({ path: "src/a.txt", status: "A", additions: 3, deletions: 0 }),
    "src/a.txt · added · +3 −0",
  );
  assert.equal(
    fileLine({ path: "b.txt", status: "M", additions: 0, deletions: 7 }),
    "b.txt · modified · +0 −7",
  );
  assert.equal(fileLine({ path: "c.txt", status: "D", additions: 0, deletions: 2 }), "c.txt · deleted · +0 −2");
  // git's fourth one-letter status, which `--no-renames` can still emit: a mode change.
  assert.equal(fileLine({ path: "run.sh", status: "T", additions: 0, deletions: 0 }), "run.sh · type changed · +0 −0");
});

test("a binary file's counts are an em dash and never a zero", () => {
  // `null` is git's `-` in `--numstat`, and the whole point of the answer carrying `null` rather than `0`: a
  // binary file changed entirely, and "no lines changed" would be the opposite of the truth.
  assert.equal(countsText(null, null), `${NO_DETAIL} ${NO_DETAIL}`);
  assert.equal(fileLine({ path: "b/bin.dat", status: "M", additions: null, deletions: null }), "b/bin.dat · modified · — —");
  const line = fileLine({ path: "b/bin.dat", status: "A", additions: null, deletions: null });
  assert.ok(!line.includes("+0") && !line.includes("−0"), `a binary file claims a count: ${line}`);
});

test("a status letter with no word is shown rather than dropped", () => {
  // An unknown letter is still git saying *something* changed; a blank cell would read as "unchanged".
  assert.equal(statusWord("X"), "X");
  assert.equal(statusWord("M"), "modified");
});

test("the counts line says there is no target rather than counting behind it", () => {
  assert.equal(countsLine(changes({ ahead: 2, behind: 1, target: "main" })), "2 ahead · 1 behind main");
  // `behind: null` is the same fact as `target: null`, so the line must not print a zero for a branch that is not
  // there — the number would be a claim about nothing.
  const none = countsLine(changes({ ahead: 2, behind: null, target: null }));
  assert.equal(none, "2 ahead · no target");
  assert.ok(!none.includes("0 behind"), `a repository with no target counted a behind: ${none}`);
});

test("the CI warning is ADR-0066's one sentence, and only when the diff touches CI", () => {
  assert.equal(ciWarning(false), null);
  const warning = ciWarning(true);
  assert.equal(
    warning,
    "this branch changes CI configuration, so publishing it may run workflows in the remote repository",
  );
  // The sentence the confirm step shows is the same value, so nothing here can drift into a second spelling.
  assert.equal(warning, CI_WARNING);
});

test("a running turn blocks the merge before a dirty tree does", () => {
  // Both facts hold at once (a running agent usually has uncommitted work), and the one worth naming is the turn:
  // an agent that is working will commit. This is `http::merge`'s order too.
  const both = changes({ dirty: true });
  assert.equal(mergeBlocked("running", both), RUNNING_MERGE_REASON);
  assert.equal(mergeBlocked("idle", both), DIRTY_MERGE_REASON);
  // The two reasons are different sentences about different facts — a test that passed with one of them deleted
  // would be the "a sentence in two places" bug wearing a new name.
  assert.notEqual(RUNNING_MERGE_REASON, DIRTY_MERGE_REASON);
  assert.ok(RUNNING_MERGE_REASON.includes("turn"), RUNNING_MERGE_REASON);
  assert.ok(DIRTY_MERGE_REASON.includes("uncommitted"), DIRTY_MERGE_REASON);
});

test("a repository with no target is blocked by the server's own sentence", () => {
  // The read already explains it (`mergeable_reason`), so the client renders it rather than writing a second
  // spelling of the configured list.
  const reason = "this project has none of the configured merge targets (main, master), so there is nothing to merge into";
  const none = changes({ target: null, behind: null, mergeable: null, mergeable_reason: reason });
  assert.equal(mergeBlocked("idle", none), reason);
  // A reason the server did not send still says something rather than disabling a button in silence.
  assert.equal(mergeBlocked("idle", changes({ target: null, mergeable_reason: null })), NO_TARGET_REASON);
});

test("a merge is blocked by running, and by nothing else a row can be", () => {
  // `409 session_running` is the only state refusal the route has (ADR-0065 D2): a `stopped` or `error` row is the
  // session an operator most wants to merge, so an `!== idle` guard here would disable the button for four of the
  // five states and this test is what would go red.
  const clean = changes();
  for (const state of ["idle", "stopped", "error", "starting"] as const) {
    assert.equal(mergeBlocked(state, clean), null, `${state} was blocked`);
  }
  assert.equal(mergeBlocked("running", clean), RUNNING_MERGE_REASON);
  assert.equal(mergeBlocked("deleting", clean), null);
  // Before the first answer there is no reason to give: the sheet is loading.
  assert.equal(mergeBlocked("idle", null), null);
});

test("the target picker offers the existing targets and preselects the answer's own", () => {
  const two = changes({ target: "develop", choices: { targets: ["develop", "main"], remotes: [] } });
  assert.deepEqual(targetOptions(two), ["develop", "main"]);
  assert.equal(preselectedTarget(two), "develop");
  // An answer with a target but no list (a shape the server does not send) still offers the one it measured.
  const bare = changes({ target: "main", choices: { targets: [], remotes: [] } });
  assert.deepEqual(targetOptions(bare), ["main"]);
  assert.equal(preselectedTarget(bare), "main");
  // No target and no list: nothing to pick, and the merge is blocked by the read's own sentence.
  const none = changes({ target: null, choices: { targets: [], remotes: [] } });
  assert.deepEqual(targetOptions(none), []);
  assert.equal(preselectedTarget(none), null);
});

test("the publish button exists exactly when a remote is configured", () => {
  assert.equal(publishVisible(changes({ choices: { targets: ["main"], remotes: [] } })), false);
  assert.equal(publishVisible(changes({ choices: { targets: ["main"], remotes: ["origin"] } })), true);
  // Not "a server is there", not "a session is idle": nothing has been read yet, so there is nothing to show.
  assert.equal(publishVisible(null), false);
});

test("the publish confirm names the branch and the remote and promises no force", () => {
  const question = publishQuestion("agent/0123456789abcdef", "origin");
  assert.ok(question.includes("agent/0123456789abcdef"), question);
  assert.ok(question.includes("origin"), question);
  // Spec item 12: the push is never forced. The confirm step is where the operator is told what they are agreeing
  // to, so the promise has to be in it.
  assert.ok(question.includes("never") || question.includes("nothing is forced"), question);
});

test("a conflict names its paths, and an empty list says git named none", () => {
  assert.equal(conflictLine(null), null);
  assert.equal(conflictLine(["a.txt"]), "conflicting: a.txt");
  assert.equal(conflictLine(["a.txt", "src/b.txt"]), "conflicting: a.txt, src/b.txt");
  const none = conflictLine([]);
  assert.ok(none !== null && none.includes("no path"), `${none}`);
  // The two shapes are different sentences: "git named none" is not "no list came back".
  assert.notEqual(conflictLine([]), conflictLine(null));
});

test("the two prompts are the sentences the ADRs fixed, and they are not each other", () => {
  // ADR-0065 D4 named the commit prompt when it built `409 dirty_worktree`; the shortcut sends that sentence, so a
  // third spelling is exactly what this test forbids.
  assert.equal(
    COMMIT_PROMPT,
    "Commit every change in the working tree, with a message that describes it, and tell me the commit's hash.",
  );
  // ADR-0064 D10 left the Resolve affordance to HS-058; item 12 says what it does, and the prompt says the same.
  assert.ok(RESOLVE_PROMPT.includes("conflicts"), RESOLVE_PROMPT);
  assert.ok(RESOLVE_PROMPT.includes("commit"), RESOLVE_PROMPT);
  assert.notEqual(COMMIT_PROMPT, RESOLVE_PROMPT);
  assert.notEqual(COMMIT_LABEL, RESOLVE_LABEL);
});

test("a merge names the target, the strategy and an abbreviated commit", () => {
  const merged = { merged_commit: "0123456789abcdef0123456789abcdef01234567", target: "main", strategy: "ff-only" };
  assert.equal(mergedNotice(merged), "merged into main by ff-only: 0123456789ab");
  // The strategy is the answer's word, not the request's: the route answers what it did.
  assert.ok(mergedNotice({ ...merged, strategy: "squash" }).includes("by squash"));
});

test("a publish names the branch, the remote and the commit", () => {
  const published = { remote: "origin", branch: "agent/0123456789abcdef", commit: "fedcba9876543210fedcba9876543210fedcba98" };
  assert.equal(publishedNotice(published), "pushed agent/0123456789abcdef to origin at fedcba987654");
});

test("a file path is encoded once, so a name with a space or a query character survives", () => {
  assert.equal(
    diffPath("0123456789abcdef", "src/a.txt"),
    "/api/sessions/0123456789abcdef/changes/diff?path=src%2Fa.txt",
  );
  // A `#` that reached the server unencoded would end the path at the fragment; a `?` would start a query.
  const odd = diffPath("0123456789abcdef", "a b?c#d&e.txt");
  assert.ok(odd.endsWith("path=a%20b%3Fc%23d%26e.txt"), odd);
  // The id is encoded too, because a route segment is not a place to trust the alphabet.
  assert.ok(diffPath("a/b", "x").startsWith("/api/sessions/a%2Fb/"), diffPath("a/b", "x"));
});

test("a sheet's failure is the envelope's code and message, even for a project refusal", () => {
  // `createFailure` drops the prefix for the create's three project codes; no route this sheet calls answers one,
  // so the sheet keeps the shape every other failure on the page has.
  assert.equal(sheetFailure("not_found", "no such file"), "not_found: no such file");
  assert.equal(sheetFailure("dirty_worktree", "the session has uncommitted work"), "dirty_worktree: the session has uncommitted work");
  assert.notEqual(sheetFailure("git_not_directory", "x"), createFailure("git_not_directory", "x"));
});

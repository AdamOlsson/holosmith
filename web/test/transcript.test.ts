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
  NO_DETAIL,
  Transcript,
  affordances,
  parseFrame,
  sessionIdFromPath,
  stoppedDetail,
  stoppedNotice,
  streamPath,
  type Change,
  type Frame,
  type SessionState,
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
    { type: "tool-end", toolId: "tool-1", output: "1  // The app" },
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
  const end = transcript.apply({ type: "tool-end", toolId: "tool-1", output: "done" });
  assert.deepEqual(end, [{ op: "tool-end", toolId: "tool-1", output: "done" }]);
  assert.deepEqual(transcript.toolOrder(), ["tool-1", "tool-2"]);
  assert.deepEqual(transcript.order(), ["m1"], "a tool call is never a message block");
});

test("a tool.end without its tool.start creates the block first", () => {
  const transcript = new Transcript();
  const changes = transcript.apply({ type: "tool-end", toolId: "tool-9", output: "the output" });
  assert.deepEqual(changes, [
    { op: "tool", toolId: "tool-9" },
    { op: "tool-end", toolId: "tool-9", output: "the output" },
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

test("every other reason is the caller's to show, so no sentence is invented for it", () => {
  // The map is deliberately one word wide: a forced abort is not data loss and a delete's own reason
  // must not be softened, so both get `null` and the badge's `state · reason` stands alone.
  for (const reason of [null, "", "forced_abort", "deleted", "idle_stop", "server_restart"]) {
    assert.equal(stoppedNotice(reason), null, String(reason));
  }
});

test("the detail line says the same thing whether the row was drawn or turned", () => {
  // HS-020's lesson from the review: the span is written twice — by `sessionRow` when the list is
  // drawn, and by `applyStatus` when a `session.status` frame turns a live row into a stopped one,
  // which is how an eviction arrives while the operator watches. One function, so they cannot differ.
  assert.equal(stoppedDetail("stopped", "evicted"), stoppedNotice("evicted"));
  assert.notEqual(stoppedDetail("stopped", "evicted"), NO_DETAIL);

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

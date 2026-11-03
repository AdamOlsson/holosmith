// The transcript's rules, folded over synthetic frames. There is no message producer until HS-015
// and no browser on the machine these gates run on, so these are the tests the session screen gets:
// the wire format HS-015 must emit, the append-or-replace rule per message id, the turn boundary,
// the composer's affordance matrix and the 10k-delta budget (ADR-0018).
//
// Run with `npm test` in `web/`: node 24's own test runner, which strips the types. No dependency and
// no lock-file change (ADR-0017). This file sits outside `src`, so `tsc --noEmit` does not typecheck
// it — typing it would mean adding `@types/node` — but the gate *runs* it, and the extension in the
// `../src/transcript.ts` import is what node's ESM resolver requires.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  Transcript,
  affordances,
  parseFrame,
  sessionIdFromPath,
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
    // A later story's kinds: the page ignores them without a release, which is the point.
    ["tool.start", '{"message_id":"m1"}'],
    ["usage", '{"in":1,"out":2}'],
    ["", ""],
  ] as const) {
    assert.equal(parseFrame(frame[0], frame[1]), null, `${frame[0]} ${frame[1]}`);
  }
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
  assert.deepEqual(affordances("running"), { prompt: false, steer: true, abort: true });
  assert.deepEqual(affordances("idle"), { prompt: true, steer: false, abort: false });
  assert.deepEqual(affordances("starting"), { prompt: false, steer: false, abort: false });
  const quiet: SessionState[] = ["stopped", "error", "deleting"];
  for (const state of quiet) {
    assert.deepEqual(affordances(state), { prompt: false, steer: false, abort: false }, state);
  }
});

test("the stream is the ADR-0014 path and no resume parameter is invented", () => {
  assert.equal(streamPath("0123456789abcdef"), "/api/sessions/0123456789abcdef/events");
  assert.ok(!streamPath("abc").includes("since="), "the browser sends Last-Event-ID, not the page");
});

test("the screen reads its id from /sessions/<id> and from nowhere else", () => {
  assert.equal(sessionIdFromPath("/sessions/0123456789abcdef"), "0123456789abcdef");
  for (const path of ["/", "/sessions", "/sessions/", "/sessions/a/b", "/api/sessions/abc", ""]) {
    assert.equal(sessionIdFromPath(path), null, path);
  }
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

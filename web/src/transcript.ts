// The transcript: what a session screen knows about the agent's output, and nothing about the DOM.
//
// The stream is the only source (ADR-0014): a delta arrives per message id, a snapshot replaces a
// message by id, a tool call arrives as its own start and end keyed by tool id, a message's usage
// arrives as its end, and a turn's start and end arrive as their own events. This file turns one SSE
// frame into the list of DOM changes it implies, so the rules can be tested without a browser by
// folding synthetic frames through it (`transcript.test.ts`, ADR-0018). It touches no `document`,
// no `fetch` and no `EventSource`.
//
// The three rules that keep the screen responsive are here rather than in the page:
//   * a message id owns exactly one block, wherever its deltas arrive;
//   * a delta is an append — never a re-read of what is already on screen, which would make one
//     turn's cost quadratic in its own length;
//   * a snapshot is a replacement by id, so receiving one after partial deltas is safe.

// The six states, spelled as the API spells them (spec item 8).
export type SessionState = "starting" | "idle" | "running" | "stopped" | "error" | "deleting";

// The two kinds a message's output arrives on. Thinking is rendered apart from text, and the spec's
// coalescing rule ("consecutive deltas of the same kind and message") is per kind.
export type DeltaKind = "text" | "thinking";

// One parsed frame of the session's stream.
export type Frame =
  | { type: "delta"; messageId: string; delta: DeltaKind; text: string }
  | { type: "snapshot"; messageId: string; text: string; thinking: string }
  | { type: "usage"; messageId: string; input: number; output: number }
  | { type: "tool-start"; toolId: string; name: string; arguments: string }
  | { type: "tool-end"; toolId: string; output: string }
  | { type: "boundary"; boundary: "start" | "end" }
  | { type: "steer"; text: string };

// What the page must do for one frame, in this order. `message` and `tool` come first when the
// block is not on screen yet, so a consumer can create the nodes the next change names.
export type Change =
  | { op: "message"; messageId: string }
  | { op: "delta"; messageId: string; delta: DeltaKind; text: string }
  | { op: "snapshot"; messageId: string; text: string; thinking: string }
  | { op: "usage"; messageId: string; input: number; output: number }
  | { op: "tool"; toolId: string }
  | { op: "tool-start"; toolId: string; name: string; arguments: string }
  | { op: "tool-end"; toolId: string; output: string }
  | { op: "boundary"; boundary: "start" | "end"; turn: number }
  | { op: "steer"; text: string };

// The SSE event names. Four are pinned by ADR-0018; `message.end`, `tool.start` and `tool.end` are
// HS-015's, and the payloads are the plan's D8.
const DELTA = "message.delta";
const SNAPSHOT = "message.snapshot";
const MESSAGE_END = "message.end";
const TOOL_START = "tool.start";
const TOOL_END = "tool.end";
const TURN_START = "turn.start";
const TURN_END = "turn.end";
// The operator's guidance reaching a running turn (HS-017). One frame, one block, never continued:
// a steer has no message id because it has nothing a second frame would update, and a reconnect
// replays the log and rebuilds the page anyway (ADR-0021).
const TURN_STEER = "turn.steer";

// The event types `parseFrame` renders, so the page subscribes to exactly these and a new kind is
// added in one place.
export const TRANSCRIPT_EVENTS: readonly string[] = [
  DELTA,
  SNAPSHOT,
  MESSAGE_END,
  TOOL_START,
  TOOL_END,
  TURN_START,
  TURN_END,
  TURN_STEER,
];

// The one id a block is keyed by. A frame that cannot name a message is not a frame: it is
// dropped rather than rendered under a blank heading.
function messageId(payload: Record<string, unknown>): string | null {
  const id = payload["message_id"];
  return typeof id === "string" && id !== "" ? id : null;
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

// A tool call's id, the key its block is held under. Like a message id, a frame that names none is
// dropped rather than rendered under a blank heading.
function toolId(payload: Record<string, unknown>): string | null {
  const id = payload["tool_id"];
  return typeof id === "string" && id !== "" ? id : null;
}

// A message's usage: the two counts `message.end` carries. A `message.end` without them names no
// usage, so it is dropped rather than rendered as `in 0 · out 0`.
function usage(payload: Record<string, unknown>): { input: number; output: number } | null {
  const counts = payload["usage"];
  if (typeof counts !== "object" || counts === null || Array.isArray(counts)) {
    return null;
  }
  const fields = counts as Record<string, unknown>;
  const input = fields["in"];
  const output = fields["out"];
  if (typeof input !== "number" || typeof output !== "number") {
    return null;
  }
  return { input, output };
}

// One SSE frame as a `Frame`, or `null` for anything this screen does not render. An unknown event
// type is not an error: HS-032, HS-033 and HS-034 add kinds (`tool.*`, usage) to the same stream,
// and a page that refused what it did not know would need a release to render a tool call.
export function parseFrame(eventType: string, data: string): Frame | null {
  if (eventType === TURN_START || eventType === TURN_END) {
    return { type: "boundary", boundary: eventType === TURN_START ? "start" : "end" };
  }
  if (
    eventType !== TURN_STEER &&
    eventType !== DELTA &&
    eventType !== SNAPSHOT &&
    eventType !== MESSAGE_END &&
    eventType !== TOOL_START &&
    eventType !== TOOL_END
  ) {
    return null;
  }
  let payload: unknown;
  try {
    payload = JSON.parse(data);
  } catch {
    return null;
  }
  if (typeof payload !== "object" || payload === null || Array.isArray(payload)) {
    return null;
  }
  const fields = payload as Record<string, unknown>;
  if (eventType === TURN_STEER) {
    // No id to read: the block is drawn from this one frame, so a payload with no text has nothing
    // to draw and is dropped like any other malformed frame.
    const guidance = text(fields["text"]);
    return guidance === null ? null : { type: "steer", text: guidance };
  }
  if (eventType === TOOL_START || eventType === TOOL_END) {
    const tool = toolId(fields);
    if (tool === null) {
      return null;
    }
    // An absent `name` or `output` is the empty string, the same rule a snapshot's kinds follow:
    // the producer said what the frame is, and the answer is "nothing to render".
    if (eventType === TOOL_START) {
      return {
        type: "tool-start",
        toolId: tool,
        name: text(fields["name"]) ?? "",
        arguments: text(fields["arguments"]) ?? "",
      };
    }
    return { type: "tool-end", toolId: tool, output: text(fields["output"]) ?? "" };
  }
  const id = messageId(fields);
  if (id === null) {
    return null;
  }
  if (eventType === MESSAGE_END) {
    const counts = usage(fields);
    return counts === null
      ? null
      : { type: "usage", messageId: id, input: counts.input, output: counts.output };
  }
  if (eventType === DELTA) {
    const kind = fields["kind"];
    if (kind !== "text" && kind !== "thinking") {
      return null;
    }
    const piece = text(fields["text"]);
    return piece === null ? null : { type: "delta", messageId: id, delta: kind, text: piece };
  }
  // An absent kind is the empty string, not "leave it alone": a snapshot is the message's whole
  // state, so a text-only snapshot clears thinking that deltas had put on screen. The same rule
  // covers a snapshot that names neither — it is an empty message, not a frame to drop, because the
  // producer said what the message is and the answer is "nothing".
  return {
    type: "snapshot",
    messageId: id,
    text: text(fields["text"]) ?? "",
    thinking: text(fields["thinking"]) ?? "",
  };
}

// The order of the blocks on screen and the number of the turn they fall in. A message block is
// created when its id is first seen and never again, so a stream that interleaves two messages keeps
// one block each and places later deltas in the block they name. A tool block is keyed by tool id and
// kept in its own order: a tool call is not a message, and a `tool.end` whose start this client never
// saw (a replay after HS-061 truncates one) creates the block first.
export class Transcript {
  private readonly blocks: string[] = [];
  private readonly seen = new Set<string>();
  private readonly tools: string[] = [];
  private readonly toolsSeen = new Set<string>();
  private turn = 0;

  // The changes `frame` implies, in the order the page must apply them.
  apply(frame: Frame): Change[] {
    if (frame.type === "boundary") {
      if (frame.boundary === "start") {
        this.turn += 1;
      }
      return [{ op: "boundary", boundary: frame.boundary, turn: this.turn }];
    }
    if (frame.type === "steer") {
      // No bookkeeping: a steer block is its own frame's, so there is no id to remember and nothing
      // a later frame could change about it (ADR-0021).
      return [{ op: "steer", text: frame.text }];
    }
    if (frame.type === "tool-start" || frame.type === "tool-end") {
      const changes: Change[] = [];
      if (!this.toolsSeen.has(frame.toolId)) {
        this.toolsSeen.add(frame.toolId);
        this.tools.push(frame.toolId);
        changes.push({ op: "tool", toolId: frame.toolId });
      }
      if (frame.type === "tool-start") {
        changes.push({
          op: "tool-start",
          toolId: frame.toolId,
          name: frame.name,
          arguments: frame.arguments,
        });
      } else {
        changes.push({ op: "tool-end", toolId: frame.toolId, output: frame.output });
      }
      return changes;
    }
    const changes: Change[] = [];
    if (!this.seen.has(frame.messageId)) {
      this.seen.add(frame.messageId);
      this.blocks.push(frame.messageId);
      changes.push({ op: "message", messageId: frame.messageId });
    }
    if (frame.type === "delta") {
      if (frame.text !== "") {
        changes.push({
          op: "delta",
          messageId: frame.messageId,
          delta: frame.delta,
          text: frame.text,
        });
      }
      return changes;
    }
    if (frame.type === "usage") {
      changes.push({
        op: "usage",
        messageId: frame.messageId,
        input: frame.input,
        output: frame.output,
      });
      return changes;
    }
    changes.push({
      op: "snapshot",
      messageId: frame.messageId,
      text: frame.text,
      thinking: frame.thinking,
    });
    return changes;
  }

  // The message ids in the order their blocks were created. The page keeps its own nodes; this
  // exists so a test can state that one message is one block.
  order(): readonly string[] {
    return this.blocks;
  }

  // The tool ids in the order their blocks were created, kept apart from `order`: a tool call is its
  // own block, never a message block.
  toolOrder(): readonly string[] {
    return this.tools;
  }
}

// Which of the composer's actions this state offers. Steer and Abort belong to a running turn
// (spec item 8's `running` row): nowhere else has anything to steer or abort. Prompting is the
// `idle` row's trigger, and `starting` is a provisioning step away from it. Resuming is the
// `stopped` and `error` rows' trigger — the transition table's only two `resume` rows (HS-018).
export type Affordances = {
  prompt: boolean;
  steer: boolean;
  abort: boolean;
  resume: boolean;
};

export function affordances(state: SessionState): Affordances {
  return {
    prompt: state === "idle",
    steer: state === "running",
    abort: state === "running",
    resume: state === "stopped" || state === "error",
  };
}

// Where the stream is. One spelling, so the list and the screen cannot ask for different paths,
// and no `since=` parameter is invented: the browser resends the last SSE `id` (a seq) as
// `Last-Event-ID` and the server replays after it (ADR-0014).
export function streamPath(id: string): string {
  return `/api/sessions/${encodeURIComponent(id)}/events`;
}

// The session a path names, or `null` for every other path. `/sessions/<id>` and nothing deeper:
// the app shell is served for any non-`/api/` path (ADR-0007), so the screen reads its own id from
// the address rather than from a router that would add a dependency.
export function sessionIdFromPath(pathname: string): string | null {
  const segments = pathname.split("/");
  // A leading slash gives an empty first segment: ["", "sessions", "<id>"].
  if (segments.length !== 3 || segments[0] !== "" || segments[1] !== "sessions") {
    return null;
  }
  const id = segments[2];
  return id === "" ? null : id;
}

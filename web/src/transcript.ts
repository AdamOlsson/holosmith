// The transcript: what a session screen knows about the agent's output, and nothing about the DOM.
//
// The stream is the only source (ADR-0014): a delta arrives per message id, a snapshot replaces a
// message by id, and a turn's start and end arrive as their own events. This file turns one SSE
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
  | { type: "boundary"; boundary: "start" | "end" };

// What the page must do for one frame, in this order. `message` comes first when the block is
// not on screen yet, so a consumer can create the nodes the next change names.
export type Change =
  | { op: "message"; messageId: string }
  | { op: "delta"; messageId: string; delta: DeltaKind; text: string }
  | { op: "snapshot"; messageId: string; text: string; thinking: string }
  | { op: "boundary"; boundary: "start" | "end"; turn: number };

// The SSE event names. They are pinned by ADR-0018: HS-015's producer must emit exactly these.
const DELTA = "message.delta";
const SNAPSHOT = "message.snapshot";
const TURN_START = "turn.start";
const TURN_END = "turn.end";

// The event types `parseFrame` renders, so the page subscribes to exactly these and a new kind is
// added in one place.
export const TRANSCRIPT_EVENTS: readonly string[] = [DELTA, SNAPSHOT, TURN_START, TURN_END];

// The one id a block is keyed by. A frame that cannot name a message is not a frame: it is
// dropped rather than rendered under a blank heading.
function messageId(payload: Record<string, unknown>): string | null {
  const id = payload["message_id"];
  return typeof id === "string" && id !== "" ? id : null;
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

// One SSE frame as a `Frame`, or `null` for anything this screen does not render. An unknown event
// type is not an error: HS-032, HS-033 and HS-034 add kinds (`tool.*`, usage) to the same stream,
// and a page that refused what it did not know would need a release to render a tool call.
export function parseFrame(eventType: string, data: string): Frame | null {
  if (eventType === TURN_START || eventType === TURN_END) {
    return { type: "boundary", boundary: eventType === TURN_START ? "start" : "end" };
  }
  if (eventType !== DELTA && eventType !== SNAPSHOT) {
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
  const id = messageId(fields);
  if (id === null) {
    return null;
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

// The order of the blocks on screen and the number of the turn they fall in. A block is created
// when its id is first seen and never again, so a stream that interleaves two messages keeps one
// block each and places later deltas in the block they name.
export class Transcript {
  private readonly blocks: string[] = [];
  private readonly seen = new Set<string>();
  private turn = 0;

  // The changes `frame` implies, in the order the page must apply them.
  apply(frame: Frame): Change[] {
    if (frame.type === "boundary") {
      if (frame.boundary === "start") {
        this.turn += 1;
      }
      return [{ op: "boundary", boundary: frame.boundary, turn: this.turn }];
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
}

// Which of the composer's actions this state offers. Steer and Abort belong to a running turn
// (spec item 8's `running` row): nowhere else has anything to steer or abort. Prompting is the
// `idle` row's trigger, and `starting` is a provisioning step away from it.
export type Affordances = { prompt: boolean; steer: boolean; abort: boolean };

export function affordances(state: SessionState): Affordances {
  return {
    prompt: state === "idle",
    steer: state === "running",
    abort: state === "running",
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

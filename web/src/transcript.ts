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

// The stream's own frame announcing that the replay it is about to send does not reach the beginning of
// the log (HS-061). Not a transcript block: it is a cursor, and it is the only frame on this stream that
// is about the stream rather than about the agent.
export const HISTORY_TRUNCATED = "history.truncated";

// How many events one "Load earlier" click asks for. The server's own default and its ceiling are its
// business; this is the page size the button sends, and it is one spelling so the fetch and the test
// cannot disagree.
export const HISTORY_PAGE_LIMIT = 200;

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

// One token count: a non-negative integer, which is what the API's own tally adds up (HS-023).
function count(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0;
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
  // A count is a count: a negative or fractional value is refused rather than rendered, and the writer
  // that adds the same numbers to the session's spend refuses exactly these (`as_u64`, HS-023). One rule
  // in two languages is a second spelling, so the two are written the same way on purpose — a screen that
  // showed `in -5` would be showing a cost the budget never counted.
  if (!count(input) || !count(output)) {
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
  // What the stream has said about cost (HS-023): the turns that closed and whether any message
  // declared what it spent. Both are facts about the log, so a replay re-establishes them.
  private settled = 0;
  private usage = false;

  // How many turns closed before the first frame this transcript will see (HS-061).
  //
  // A turn is numbered from the `turn.start` frames a client has seen, which is true of a whole log and
  // of a windowed one only if the window says how much it skipped. A replay that starts at the 50th most
  // recent message would otherwise label its first turn "turn 1" while the operator's log says turn 40.
  // The frame is sent before any transcript frame, and a page is folded through a fresh model, so this is
  // always set before the first boundary arrives.
  setTurnBase(turnsBefore: number): void {
    this.turn = turnsBefore;
  }

  // The changes `frame` implies, in the order the page must apply them.
  apply(frame: Frame): Change[] {
    if (frame.type === "boundary") {
      if (frame.boundary === "start") {
        this.turn += 1;
      } else {
        this.settled += 1;
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
      // A `message.end` without usage never becomes a frame (`parseFrame` drops it), so reaching here
      // is the only way the model learns the agent said what a message cost.
      this.usage = true;
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

  // What the stream has said about cost, for `budgetNotice`.
  spend(): Spend {
    return { settled: this.settled, usage: this.usage };
  }
}

// What the stream has said about what the agent spent (HS-023): how many turns have closed, and
// whether any message declared what it cost.
export type Spend = { settled: number; usage: boolean };

// The sentence a session screen says when the backend told it nothing about usage, or `null` when it
// has nothing to say.
//
// The token budget counts `usage_in + usage_out`, which the writer accumulates from `message.end`
// (HS-023); a backend that sends no `usage` leaves the budget nothing to count, and spec item 13's
// contingency says the operator is told so rather than shown a zero. Two facts decide it, and neither
// is the row's total: a *settled turn* is what makes usage expected at all ("no turn has run yet" is
// not "the backend reports nothing"), and *a usage frame* is what makes it reported. Both are read
// from the log, which the page replays on every load, so the sentence is the same after a reload as it
// was live — and a row whose two columns are 0 cannot say which of the two cases it is in. A replay
// truncated to the newest messages (HS-061) carries the settled turns *inside* the window, which is
// why `history.truncated`'s `turns_before` exists: a turn count that started at zero would say "no turn
// has run yet" about a session that has run forty.
export function budgetNotice(spend: Spend): string | null {
  if (spend.settled === 0 || spend.usage) {
    return null;
  }
  return "no usage reported — the token budget has nothing to count";
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

// How much of a reason the badge shows, in characters.
//
// A reason is normally one word (`evicted`, `idle_stop`) or one short sentence, and fits. An unexpected
// exit's is not: it is the last few lines the container printed (HS-030, spec item 6), so the badge shows
// the *last* of them — the error itself, not the oldest line of the tail — on one line and clipped.
// `session.log` keeps every line; the badge has one ellipsis' worth of room.
export const REASON_MAX = 48;

// A reason as one line a badge can hold: its last non-empty line, clipped.
export function headline(reason: string): string {
  const lines = reason
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
  // A reason with no content at all keeps what it was given: an empty badge is the producer's answer, not
  // this function's to invent.
  const text = lines.length === 0 ? reason.trim() : lines[lines.length - 1]!;
  return text.length > REASON_MAX ? `${text.slice(0, REASON_MAX)}…` : text;
}

// What a row's badge says: the state, and its reason when there is one.
//
// One function because the badge is written in five places — the list row, the screen's badge, and the
// three paths a `session.status` frame takes — and because a reason's *shape* is the renderer's business:
// the row keeps what happened, and this decides how much of it a phone shows.
export function stateText(state: SessionState, reason: string | null): string {
  return reason === null ? state : `${state} · ${headline(reason)}`;
}

// What a stopped session's row says beyond its state, or `null` when its reason needs no sentence.
//
// Two reasons earn one, and they are the two the machine writes without being asked: `evicted` is a
// create stopping the least recently active idle session to take its slot (HS-020), and `idle_stop` is
// a session left alone for longer than `idle_stop_min` (HS-021). Spec item 9 is emphatic that neither
// costs anything — "history, worktree, events and images are kept. Nothing is deleted by the cap." — so
// the row says so, because a badge reading `stopped · evicted` alone reads like a loss. The two
// sentences differ in *why* (making room, or sitting idle) and share the promise that matters, which
// is why they stay in one function. Every other reason is the caller's to show as it is: a forced
// abort and a delete's own word are not this sentence, and inventing a reassuring line for them would
// be a claim about work nobody preserved.
export function stoppedNotice(reason: string | null): string | null {
  if (reason === "evicted") {
    return "stopped to make room — nothing was deleted, resume it to continue";
  }
  if (reason === "idle_stop") {
    return "stopped after sitting idle — nothing was deleted, resume it to continue";
  }
  return null;
}

// What the row's detail span holds: the notice when the state and its reason earn one, and a dash
// otherwise. One function because the span is written in two places — when the row is built, and when
// a `session.status` frame turns a live row into a stopped one, which is how an eviction (HS-020) or
// an idle stop (HS-021) arrives — and two spellings there would show the operator different things
// depending on whether they were watching.
export const NO_DETAIL = "—";

export function stoppedDetail(state: SessionState, reason: string | null): string {
  if (state !== "stopped") {
    return NO_DETAIL;
  }
  return stoppedNotice(reason) ?? NO_DETAIL;
}

// What a create's refusal says in the form's error area (HS-026).
//
// Spec item 1's three refusals are the project's, not the request's: the operator has to be told what is
// wrong with the repository, and a `git_not_directory:` prefix in front of that sentence is a machine
// word the phone cannot act on. The story asks for exactly this — "the UI explains the refusal in one
// sentence rather than showing a code" — so the three codes drop the prefix and every other answer keeps
// the `code: message` shape the rest of the page uses.
export const PROJECT_REFUSALS: readonly string[] = [
  "git_not_directory",
  "submodules_present",
  "worktree_config_enabled",
];

export function createFailure(code: string, message: string): string {
  return PROJECT_REFUSALS.includes(code) ? message : `${code}: ${message}`;
}

// Where the stream is. One spelling, so the list and the screen cannot ask for different paths,
// and no `since=` parameter is invented: the browser resends the last SSE `id` (a seq) as
// `Last-Event-ID` and the server replays after it (ADR-0014).
export function streamPath(id: string): string {
  return `/api/sessions/${encodeURIComponent(id)}/events`;
}

// The `history.truncated` frame's payload: where the replay starts, and how many turns it skipped.
//
// Two keys, and the second is the one a client cannot compute: the frames that would have said how many
// turns came before are exactly the ones it was not sent.
export type Truncated = { beforeSeq: number; turnsBefore: number };

function positive(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value > 0;
}

// One `history.truncated` frame, or `null` for a payload that names no cursor. A frame with no usable
// `before_seq` has nothing to offer a page request, so it is dropped like any other malformed frame — and
// a missing `turns_before` is 0 rather than a refusal, because a replay with no turn numbering is wrong
// about labels and not about content.
export function parseTruncated(data: string): Truncated | null {
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
  const beforeSeq = fields["before_seq"];
  if (!positive(beforeSeq)) {
    return null;
  }
  const turnsBefore = fields["turns_before"];
  return {
    beforeSeq,
    turnsBefore: typeof turnsBefore === "number" && Number.isInteger(turnsBefore) && turnsBefore >= 0
      ? turnsBefore
      : 0,
  };
}

// One page of older history, as `GET /api/sessions/:id/events/history` answers it (HS-061).
//
// `events` are stored rows, so each is a `type`/`payload` pair `parseFrame` already understands; the
// three other fields are the cursor, whether another page exists, and how many turns closed before this
// one — the same number the truncation frame carries, for the same reason.
export type HistoryPage = {
  events: { seq: number; type: string; payload: string; created_at: number }[];
  before_seq: number;
  has_more: boolean;
  turns_before: number;
};

// Where one older page lives. `before` is exclusive and is the cursor the truncation frame or the previous
// page named, so a client never computes a seq of its own.
export function historyPath(id: string, before: number, limit: number = HISTORY_PAGE_LIMIT): string {
  const query = `before=${before}&limit=${limit}`;
  return `/api/sessions/${encodeURIComponent(id)}/events/history?${query}`;
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

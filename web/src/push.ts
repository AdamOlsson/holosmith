// The app's installability and notification rules (HS-067, spec item 11; ADR-0072).
//
// This module is the **one** home of every string and every branch the browser half of item 11 shows or takes:
// the four notification sentences `web/src/sw.ts` draws in a worker, the "which path does this tap open" rule,
// the settings screen's one control, and the iOS install sentence. `npm test` is the only gate that can see a
// sentence and no gate may need a browser (ADR-0067), so `main.ts` and `sw.ts` only turn these answers into DOM
// and into `self.registration.showNotification` calls — neither of them decides anything.
//
// No state, no I/O, no DOM: every function here takes plain values, which is also what makes the worker (which
// has no DOM) and the page (which has no worker scope) able to share them.


// The control origin's own paths. `SETTINGS_PATH` is a **third screen**, not a section of one: the subscription
// belongs to the machine, and a section of the session screen would be unreachable on a machine with no sessions
// (ADR-0072 D2). The server needs no route for it — the app shell is answered for every non-`/api/` path
// (ADR-0007) — so this is the page's own path and nothing else's.
export const SETTINGS_PATH = "/settings";

// The Computers screen (HS-071): the fourth screen, and the one that is not about this machine alone. Its path is
// here for `SETTINGS_PATH`'s reason — the header's list of screens is the only thing that has to know all of them,
// and a path spelled in two places is a link that 404s on one of them.
export const COMPUTERS_PATH = "/computers";

// The two push routes HS-066 built. The page never spells either anywhere else.
export const SUBSCRIBE_PATH = "/api/push/subscribe";

// Spec item 11's four `kind` words, and the sentence each one earns. The payload is content-free by design
// (ADR-0071), so these four strings are the whole of what a notification can say.
const KIND_SENTENCES: Readonly<Record<string, string>> = {
  turn_finished: "a turn finished",
  error: "a session failed",
  turn_timeout: "a turn timed out",
  budget_exceeded: "a turn ran out of its budget",
};

// The one sentence a word this build does not know earns. A worker that threw instead would end that
// subscription's delivery silently — the browser drops a `push` handler's failure — so an unknown kind is still
// a notification, and the tap still opens the session.
export const UNKNOWN_KIND = "there is news about a session";

// What a notification says, from the payload's `kind`. A word this build does not know earns the generic sentence
// rather than an exception, which is the whole rule (ADR-0072 D4).
export function notificationText(kind: string): string {
  return KIND_SENTENCES[kind] ?? UNKNOWN_KIND;
}

// The notification's title is the payload's `host` — `local` for a server's own session (GLOSSARY's **Local**).
export function notificationTitle(host: string): string {
  return host === "" ? "Holosmith" : `Holosmith · ${host}`;
}

// One notification per session: two sessions must not replace each other on a lock screen. Gated on the same
// sixteen-hex rule as the path, so a tag cannot carry arbitrary text either.
export function notificationTag(sessionId: string): string {
  return `holosmith-${isSessionId(sessionId) ? sessionId : "unknown"}`;
}

// A session id is sixteen lowercase hex characters (`sessions::is_id`). It arrives in a payload a push service
// delivered, so it is untrusted input on its way into a URL.
export function isSessionId(id: string): boolean {
  return /^[0-9a-f]{16}$/.test(id);
}

// Which path a tap opens: the session the payload names, or the sessions list for anything else. A path built
// from arbitrary text is a link this machine did not author, and `openWindow` would open it in the app's own
// origin — so the rule is the id's alphabet and nothing else (ADR-0072 D7).
export function notificationPath(sessionId: string): string {
  return isSessionId(sessionId) ? `/sessions/${sessionId}` : "/";
}

// The notification's payload as the sender writes it: exactly `{host, session_id, kind}` (ADR-0071). `null` for
// anything else — a body that is not this shape is not something a worker may guess at.
export type NotificationPayload = { host: string; sessionId: string; kind: string };

export function notificationPayload(value: unknown): NotificationPayload | null {
  if (typeof value !== "object" || value === null) {
    return null;
  }
  const record = value as Record<string, unknown>;
  const host = record["host"];
  const sessionId = record["session_id"];
  const kind = record["kind"];
  if (typeof host !== "string" || typeof sessionId !== "string" || typeof kind !== "string") {
    return null;
  }
  return { host, sessionId, kind };
}

// What `pushManager.subscribe` asks the server to store: `{endpoint, p256dh, auth}`.
//
// The browser hands back `{endpoint, keys: {p256dh, auth}}` (`PushSubscription::toJSON`), so the two key fields
// have to be lifted out of `keys` — the server's route takes them at the top level (HS-066) and a body with a
// nested `keys` object is `422 bad_body`. Keys the browser gives as `null` are left out rather than sent as
// null: the route reads a missing field as the body's shape, which is the honest answer.
export type SubscriptionBody = { endpoint: string; p256dh: string; auth: string };

export function subscriptionBody(value: unknown): SubscriptionBody | null {
  if (typeof value !== "object" || value === null) {
    return null;
  }
  const record = value as Record<string, unknown>;
  const endpoint = record["endpoint"];
  const keys = record["keys"];
  if (typeof endpoint !== "string" || endpoint === "") {
    return null;
  }
  if (typeof keys !== "object" || keys === null) {
    return null;
  }
  const { p256dh, auth } = keys as Record<string, unknown>;
  if (typeof p256dh !== "string" || typeof auth !== "string") {
    return null;
  }
  return { endpoint, p256dh, auth };
}

// ---- the settings screen's one control ---------------------------------------------------------------
//
// Five arms and one order, which is the story: what the browser can do, then what the operator has decided, then
// what this machine can deliver, then what is stored.

export type PushAction = "subscribe" | "unsubscribe" | "blocked" | "unsupported" | "no-key";

// The facts the screen is drawn from, each read in `main.ts` from the platform.
export type PushFacts = {
  // `navigator.serviceWorker` and `PushManager` both exist, and the origin is secure (a `https://` page or
  // localhost). An insecure origin has `serviceWorker` at all, so this one boolean covers all three.
  supported: boolean;
  // `Notification.permission`.
  permission: NotificationPermission;
  // This machine answered `application_server_key`: without one, a subscribe cannot be made at all.
  hasKey: boolean;
  // `registration.pushManager.getSubscription() !== null`.
  subscribed: boolean;
  // Whether the page is running as the installed app (`display-mode: standalone`, or iOS's `navigator.standalone`).
  installed: boolean;
};

export type PushState = { action: PushAction; sentence: string; detail: string | null };

// Which control the screen offers, and the sentence beside it.
//
// The order is load-bearing and is the arms' own justification: a browser that cannot push is told that before
// anything else; **an existing subscription can always be undone** — even on a machine whose pair has gone, and
// even after the operator blocked notifications, because a row this machine still holds is a row it will keep
// trying to deliver to; then a permission the operator refused, which only their browser's settings can undo; then
// a machine with no pair, whose sentence names the command that writes one.
//
// `subscribed` before `denied` is the arm that is easy to get wrong and was: a browser that accepted a subscription
// and was then told to block notifications still has that subscription, so the screen has to offer the one thing
// that removes it. The blocked fact is not lost — it becomes the `detail` under `subscribed`.
const BLOCKED_DETAIL =
  "this browser remembers a refusal and will not ask again: allow notifications for this site in the browser's settings, then reload";

export function pushState(facts: PushFacts): PushState {
  if (!facts.supported) {
    return {
      action: "unsupported",
      sentence: "this browser cannot receive notifications",
      detail:
        "push needs a secure origin (https, or localhost) and a browser with service-worker support; on iOS it needs 16.4 or later",
    };
  }
  if (facts.subscribed) {
    return {
      action: "unsubscribe",
      sentence: "subscribed on this machine",
      detail: facts.permission === "denied" ? BLOCKED_DETAIL : null,
    };
  }
  if (facts.permission === "denied") {
    return {
      action: "blocked",
      sentence: "notifications are blocked for this app",
      detail: BLOCKED_DETAIL,
    };
  }
  if (!facts.hasKey) {
    return {
      action: "no-key",
      sentence: "this machine has no Web Push key pair",
      detail: "`server deploy` writes it once; until then there is nothing to subscribe to",
    };
  }
  return {
    action: "subscribe",
    sentence: "not subscribed on this machine",
    detail: null,
  };
}

// The button's own word, so the label and the action cannot disagree.
export function pushActionLabel(action: PushAction): string {
  switch (action) {
    case "subscribe":
      return "Subscribe";
    case "unsubscribe":
      return "Unsubscribe";
    case "blocked":
      return "Blocked";
    case "unsupported":
      return "Unavailable";
    case "no-key":
      return "No key";
  }
}

// Whether the button is offered at all: the two arms that cannot be acted on from this page are shown as their
// sentence and nothing else, and a re-render during a request must not re-enable it (ADR-0067's `busy` lesson).
export function pushActionable(action: PushAction): boolean {
  return action === "subscribe" || action === "unsubscribe";
}

// ---- the iOS sentence ------------------------------------------------------------------------------
//
// Item 11's third bullet: iOS delivers a push only to an app on the home screen, and that requirement is stated
// in the UI rather than in a document. Two sentences, because the requirement is iOS's and a desktop browser's
// offer is a convenience — and neither is shown once the app really is installed, where the state row says so.

export const IOS_INSTALL_SENTENCE =
  "iOS delivers notifications only to an app on the home screen: tap Share, then Add to Home Screen, and open Holosmith from the icon";

export const INSTALL_HINT =
  "you can add Holosmith to your home screen from this browser's menu";

export function installSentence(facts: { installed: boolean; ios: boolean }): string | null {
  if (facts.installed) {
    return null;
  }
  return facts.ios ? IOS_INSTALL_SENTENCE : INSTALL_HINT;
}

// iOS's own detection: the phone and pad user agents, and the iPad since iOS 13, which reports a Macintosh user
// agent and is only told apart by its touch points.
export function isIos(userAgent: string, maxTouchPoints: number): boolean {
  if (/iPhone|iPad|iPod/.test(userAgent)) {
    return true;
  }
  return userAgent.includes("Macintosh") && maxTouchPoints > 1;
}

// The per-machine row's sentence. One origin today, so one row — and the second line is why a second machine
// will be a second subscription: each machine signs with its own pair and seals to its own key (item 11's last
// bullet).
export function machineState(origin: string, subscribed: boolean): string {
  return `${origin} · ${subscribed ? "subscribed" : "not subscribed"}`;
}

export const PER_MACHINE_SENTENCE =
  "each machine has its own Web Push key pair, so a phone subscribes on every machine it uses";

// ---- the header's links -------------------------------------------------------------------------

// Where the header's links go, and what they say. `main.ts` draws them on every screen, so the rule is here
// rather than in the DOM.
//
// One link per **other** screen, and never one to the page you are on: a link to the page you are reading is a
// no-op the operator has to read before ignoring it (HS-067's rule). Since HS-071 there are three screens — the
// sessions list at `/`, the Computers screen at `COMPUTERS_PATH`, and this one — so the header carries two links
// where it carried one, and this is the reason `case_pwa`'s `id="nav-link"` needle became a class name.
export type NavLink = { href: string; label: string };

// Every screen the app serves, in the order the header lists them.
export const SCREENS: readonly NavLink[] = [
  { href: "/", label: "Sessions" },
  { href: COMPUTERS_PATH, label: "Computers" },
  { href: SETTINGS_PATH, label: "Notifications" },
];

export function navLinks(pathname: string): NavLink[] {
  return SCREENS.filter((screen) => screen.href !== pathname);
}

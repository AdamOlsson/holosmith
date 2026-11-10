// The installable app's rules (HS-067, spec item 11; ADR-0072), tested without a browser.
//
// `npm test` is the only gate that can see a sentence, and no gate may need a browser, so everything the settings
// screen and the service worker decide is a pure function in `web/src/push.ts` and is asserted here: the four
// notification sentences, the untrusted-payload and untrusted-id rules, the flattened subscription body, the one
// control per browser state, and the iOS install sentence. The *files* are asserted too — the manifest parses and
// every icon it names is a PNG of the size it claims — because those are bytes no shell `grep` can measure.
//
// Run with `npm test` in `web/`: node 24's own runner, which strips the types. Like the transcript's suite this
// file sits outside `src`, so `tsc --noEmit` does not typecheck it.

import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";

import {
  INSTALL_HINT,
  IOS_INSTALL_SENTENCE,
  PER_MACHINE_SENTENCE,
  COMPUTERS_PATH,
  SETTINGS_PATH,
  SUBSCRIBE_PATH,
  UNKNOWN_KIND,
  installSentence,
  isIos,
  isSessionId,
  machineState,
  machineSubscription,
  navLinks,
  notificationData,
  subscriptionRoute,
  SCREENS,
  notificationPath,
  notificationPayload,
  notificationTag,
  notificationText,
  notificationTitle,
  pushActionLabel,
  pushActionable,
  pushState,
  subscriptionBody,
  type PushFacts,
} from "../src/push.ts";
import { apiPath, sessionHref } from "../src/transcript.ts";

// One well-formed session id: sixteen lowercase hex characters (`sessions::is_id`).
const ID = "0123456789abcdef";

// The facts a browser that can do everything reports, so each test can flip one.
function facts(overrides: Partial<PushFacts> = {}): PushFacts {
  return {
    supported: true,
    permission: "granted",
    hasKey: true,
    subscribed: false,
    installed: false,
    away: null,
    ...overrides,
  };
}

// ---- the notification's text ----------------------------------------------------------------------

test("each kind word has its own sentence, and no two are the same", () => {
  const sentences = ["turn_finished", "error", "turn_timeout", "budget_exceeded"].map(notificationText);
  assert.deepEqual(sentences, [
    "a turn finished",
    "a session failed",
    "a turn timed out",
    "a turn ran out of its budget",
  ]);
  // Four distinct sentences: a copy-paste that left two kinds saying one thing is the failure this line catches.
  assert.equal(new Set(sentences).size, 4);
});

test("a kind this build does not know is a notification and never an exception", () => {
  // A throw in a `push` handler ends that subscription's delivery silently, so an unknown word is the generic
  // sentence — and the tap still opens the session, which `notificationPath` is asked about separately.
  assert.equal(notificationText("something_new"), UNKNOWN_KIND);
  assert.equal(notificationText(""), UNKNOWN_KIND);
  assert.notEqual(notificationText("turn_finished"), UNKNOWN_KIND);
});

test("the title is the payload's host, and a nameless host still says what this is", () => {
  // `local` is the reserved word for *the machine whose page this is* (GLOSSARY's **Local**), so it earns the bare
  // name: a phone has no registry to look a word like that up in, and "local" is not a machine's name.
  assert.equal(notificationTitle("local"), "Holosmith");
  assert.equal(notificationTitle("build-box"), "Holosmith · build-box");
  assert.equal(notificationTitle(""), "Holosmith");
  assert.notEqual(notificationTitle("build-box"), notificationTitle("workshop"));
});

test("one notification per session, so two sessions do not replace each other", () => {
  assert.equal(notificationTag(ID), `holosmith-${ID}`);
  assert.notEqual(notificationTag("0000000000000001"), notificationTag("0000000000000002"));
  // A tag is not a way to smuggle arbitrary text into the notification's identity either.
  assert.equal(notificationTag("<script>"), "holosmith-unknown");
});

// ---- the id is untrusted input --------------------------------------------------------------------

test("a session id is sixteen lowercase hex characters and nothing else", () => {
  assert.equal(isSessionId(ID), true);
  assert.equal(isSessionId("0000000000000000"), true);
  // Uppercase is a different string that the server's table does not hold (`sessions::is_id`).
  assert.equal(isSessionId("0123456789ABCDEF"), false);
  assert.equal(isSessionId("0123456789abcde"), false, "fifteen characters");
  assert.equal(isSessionId("0123456789abcdef0"), false, "seventeen characters");
  assert.equal(isSessionId("0123456789abcdeZ"), false);
  assert.equal(isSessionId(""), false);
  // The shapes a payload could carry if something else wrote it.
  assert.equal(isSessionId("../etc/passwd"), false);
  assert.equal(isSessionId("0123456789abcde/"), false);
  assert.equal(isSessionId("0123456789abcde?"), false);
  assert.equal(isSessionId(" 0123456789abcde"), false);
});

test("a tap opens the named session, and anything else opens the list", () => {
  assert.equal(notificationPath("local", ID), `/sessions/${ID}`);
  // Not a path built from arbitrary text: `openWindow` would open it on this app's own origin.
  assert.equal(notificationPath("local", "../../api/push/subscribe"), "/");
  assert.equal(notificationPath("local", ""), "/");
  assert.equal(notificationPath("local", "//evil.example/"), "/");
  assert.equal(notificationPath("local", "0123456789ABCDEF"), "/");
});

test("the payload's host is a path segment, and only a registry name gets one", () => {
  // The machine the session lives on (HS-074): the hub's own forwarding route, which is what makes the tap open
  // *that* machine's session rather than this page's own.
  assert.equal(notificationPath("workshop", ID), `/hosts/workshop/sessions/${ID}`);
  assert.equal(notificationPath("build-box-2", ID), `/hosts/build-box-2/sessions/${ID}`);
  // An empty host is this machine, the way `local` is.
  assert.equal(notificationPath("", ID), `/sessions/${ID}`);
  // `hosts::name_problem`'s alphabet, and nothing else: a name is one path segment, so a slash, a dot-segment, a
  // query or a scheme is not a name a registry row could take either.
  for (const host of [
    "../sessions",
    "workshop/../sessions",
    "workshop?sessions",
    "https://workshop",
    "//evil.example",
    "Work Shop",
    "-workshop",
    "workshop-",
    "workshop.",
    "a".repeat(64),
  ]) {
    assert.equal(notificationPath(host, ID), "/", host);
  }
});

test("the tap's path and the screen's path are the same rule, spelled twice", () => {
  // `push.ts` cannot import `transcript.ts` (`npm test`'s ESM resolver wants an extension on every hop, HS-071), so
  // the two spellings are compared here rather than merged. This is the test that makes the second spelling safe.
  // The table is exactly what both are called with in practice: `local` and names a registry row can hold. A name
  // that could not be a row — `""`, `"../evil"`, `"Work Shop"` — is `notificationPath`'s own business and is
  // refused there (the test above), while `sessionHref` would percent-encode it; comparing the two there would be
  // comparing two different questions.
  for (const host of ["local", "workshop", "build-box-2", "a".repeat(63)]) {
    assert.equal(notificationPath(host, ID), sessionHref(host, ID), host);
  }
});

// ---- the payload's shape --------------------------------------------------------------------------

test("the payload is the sender's three keys", () => {
  assert.deepEqual(notificationPayload({ host: "local", session_id: ID, kind: "turn_finished" }), {
    host: "local",
    sessionId: ID,
    kind: "turn_finished",
  });
  // A future additive key does not make the payload unreadable — the three this build needs are there.
  assert.deepEqual(
    notificationPayload({ host: "local", session_id: ID, kind: "error", extra: 1 }),
    { host: "local", sessionId: ID, kind: "error" },
  );
});

test("a body that is not this shape is null rather than a guess", () => {
  for (const value of [
    null,
    undefined,
    "local",
    42,
    [],
    {},
    { host: "local", session_id: ID },
    { host: "local", kind: "error" },
    { session_id: ID, kind: "error" },
    { host: 1, session_id: ID, kind: "error" },
    { host: "local", session_id: null, kind: "error" },
    { host: "local", session_id: ID, kind: ["error"] },
  ]) {
    assert.equal(notificationPayload(value), null, JSON.stringify(value));
  }
});

// ---- the subscription body ------------------------------------------------------------------------

test("the browser's nested keys are lifted to the route's two top-level fields", () => {
  const body = subscriptionBody({
    endpoint: "https://push.example/abc",
    expirationTime: null,
    keys: { p256dh: "BP4z", auth: "BTBZ" },
  });
  assert.deepEqual(body, {
    endpoint: "https://push.example/abc",
    p256dh: "BP4z",
    auth: "BTBZ",
  });
  // Exactly the route's three keys: `{"endpoint","p256dh","auth"}` is what `http/push.rs` parses, and a nested or
  // extra key would be its `bad_body`.
  assert.deepEqual(Object.keys(body ?? {}).sort(), ["auth", "endpoint", "p256dh"]);
});

test("a subscription without an endpoint or a key pair is null", () => {
  for (const value of [
    null,
    {},
    { keys: { p256dh: "x", auth: "y" } },
    { endpoint: "" , keys: { p256dh: "x", auth: "y" } },
    { endpoint: "https://push.example/a" },
    { endpoint: "https://push.example/a", keys: {} },
    { endpoint: "https://push.example/a", keys: { p256dh: "x" } },
    { endpoint: "https://push.example/a", keys: { p256dh: "x", auth: 3 } },
  ]) {
    assert.equal(subscriptionBody(value), null, JSON.stringify(value));
  }
});

// ---- the settings screen's one control ------------------------------------------------------------

test("the control the screen offers, one arm at a time", () => {
  assert.deepEqual(pushState(facts()).action, "subscribe");
  assert.deepEqual(pushState(facts({ subscribed: true })).action, "unsubscribe");
  assert.deepEqual(pushState(facts({ permission: "denied" })).action, "blocked");
  assert.deepEqual(pushState(facts({ supported: false })).action, "unsupported");
  assert.deepEqual(pushState(facts({ hasKey: false })).action, "no-key");
  // A machine that did not answer (HS-074): its own sentence, and no control.
  assert.deepEqual(pushState(facts({ away: "workshop could not be reached" })).action, "away");
});

test("the arms' order is what makes the sentences true", () => {
  // A browser that cannot push is told that before anything else, even if the other facts are odd.
  assert.equal(pushState(facts({ supported: false, permission: "denied", subscribed: true })).action, "unsupported");
  // A refusal the browser remembers cannot be undone from this page, so no button is offered.
  assert.equal(pushState(facts({ permission: "denied", subscribed: false })).action, "blocked");
  // A subscription can always be undone — even on a machine whose pair has gone, which is exactly when an
  // operator needs the button.
  assert.equal(pushState(facts({ subscribed: true, hasKey: false })).action, "unsubscribe");
  // And even after the operator blocked notifications: the browser still holds the subscription, so a row this
  // machine keeps delivering to is a row the screen has to be able to remove. The refusal is not lost — it is the
  // arm's detail.
  const blockedButSubscribed = pushState(facts({ subscribed: true, permission: "denied" }));
  assert.equal(blockedButSubscribed.action, "unsubscribe");
  assert.match(blockedButSubscribed.detail ?? "", /browser's settings/);
  // The two states that only *look* similar: a refusal with nothing stored offers no button at all.
  assert.equal(pushState(facts({ permission: "denied" })).action, "blocked");
  assert.equal(pushState(facts({ permission: "denied" })).detail, blockedButSubscribed.detail);
  // No pair means no subscribe, and the sentence names the command that writes one.
  const keyless = pushState(facts({ hasKey: false }));
  assert.equal(keyless.action, "no-key");
  assert.match(keyless.detail ?? "", /server deploy/);
});

test("every arm has a sentence of its own, and a detail only where one is earned", () => {
  const states = [
    pushState(facts()),
    pushState(facts({ subscribed: true })),
    pushState(facts({ permission: "denied" })),
    pushState(facts({ supported: false })),
    pushState(facts({ hasKey: false })),
    pushState(facts({ away: "workshop could not be reached" })),
  ].map((state) => state.sentence);
  assert.equal(new Set(states).size, 6, states.join(" | "));
  // `subscribed` and `not subscribed` are the only two arms with nothing to add: the button already says it.
  assert.equal(pushState(facts()).detail, null);
  assert.equal(pushState(facts({ subscribed: true })).detail, null);
  // The blocked arm names where the refusal can be undone, because nothing on this page can.
  assert.match(pushState(facts({ permission: "denied" })).detail ?? "", /browser's settings/);
});

test("only the two arms that can be acted on are offered as buttons", () => {
  assert.equal(pushActionable("subscribe"), true);
  assert.equal(pushActionable("unsubscribe"), true);
  for (const action of ["blocked", "unsupported", "no-key", "away"] as const) {
    assert.equal(pushActionable(action), false, action);
  }
});

test("each action's label is its own word", () => {
  const labels = (["subscribe", "unsubscribe", "blocked", "unsupported", "no-key", "away"] as const).map(
    pushActionLabel,
  );
  assert.equal(new Set(labels).size, 6, labels.join(" | "));
  assert.equal(pushActionLabel("subscribe"), "Subscribe");
  assert.equal(pushActionLabel("unsubscribe"), "Unsubscribe");
});

// ---- the iOS sentence ------------------------------------------------------------------------------

test("iOS's home-screen requirement is a sentence the UI shows, not a document", () => {
  assert.equal(installSentence({ installed: false, ios: true }), IOS_INSTALL_SENTENCE);
  assert.match(IOS_INSTALL_SENTENCE, /home screen/);
  assert.match(IOS_INSTALL_SENTENCE, /iOS/);
  // A browser that is not iOS gets the ordinary offer, which is a different sentence.
  assert.equal(installSentence({ installed: false, ios: false }), INSTALL_HINT);
  assert.notEqual(INSTALL_HINT, IOS_INSTALL_SENTENCE);
  // And once the app really is installed there is nothing to say: the row above already says `subscribed`.
  assert.equal(installSentence({ installed: true, ios: true }), null);
  assert.equal(installSentence({ installed: true, ios: false }), null);
});

test("the install sentence is not shown in place of a state", () => {
  // Both sentences are about installing, and neither may be mistaken for the subscription's own state.
  for (const sentence of [IOS_INSTALL_SENTENCE, INSTALL_HINT]) {
    assert.doesNotMatch(sentence, /subscribed/);
  }
});

test("iOS is the phone and pad agents, and the iPad that pretends to be a Macintosh", () => {
  const iphone =
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
  const ipad =
    "Mozilla/5.0 (iPad; CPU OS 16_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.4 Mobile/15E148 Safari/604.1";
  // iPadOS 13 and later report a desktop Macintosh agent; the touch points are the only thing that tells it apart.
  const ipadDesktop =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_6) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
  const mac =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";
  const android =
    "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36";
  assert.equal(isIos(iphone, 5), true);
  assert.equal(isIos(ipad, 5), true);
  assert.equal(isIos(ipadDesktop, 5), true);
  assert.equal(isIos(mac, 0), false, "a Macintosh with no touch points is a desktop");
  assert.equal(isIos(android, 5), false);
  assert.equal(isIos("", 0), false);
});

// ---- the per-machine row and the header's link -----------------------------------------------------

test("the machine's row names the machine and whether it is subscribed", () => {
  assert.equal(machineState("workshop", "subscribed"), "workshop · subscribed");
  assert.equal(machineState("workshop", "not-subscribed"), "workshop · not subscribed");
  // A machine that did not answer says so, in GLOSSARY's **Reach** word: `not subscribed` would be a claim about a
  // machine nothing measured.
  assert.equal(machineState("workshop", "unreachable"), "workshop · unreachable");
  // The sentence that explains why a second machine is a second subscription is not the row's, and says so.
  assert.match(PER_MACHINE_SENTENCE, /each machine has its own/);
});

test("the header offers every other screen and never itself", () => {
  // Three screens since HS-071, so two links on every page — and one fewer than the screens on each.
  for (const screen of SCREENS) {
    const links = navLinks(screen.href);
    assert.equal(links.length, SCREENS.length - 1, screen.href);
    // A link to the page you are on is a no-op the operator has to read before ignoring it.
    assert.ok(!links.some((link) => link.href === screen.href), screen.href);
    // Every screen is reachable from every other one: no page is a dead end.
    for (const other of SCREENS) {
      if (other.href !== screen.href) {
        assert.ok(
          links.some((link) => link.href === other.href),
          `${other.href} is not linked from ${screen.href}`,
        );
      }
    }
  }
  // A session's page is not a screen of this list, so it is offered every screen there is — and a path that is not a
  // screen at all gets the same answer, because the rule compares against `SCREENS` and never against the path.
  for (const pathname of [`/sessions/${ID}`, `/hosts/workshop/sessions/${ID}`, "/nonsense"]) {
    assert.deepEqual(
      navLinks(pathname).map((link) => link.href),
      ["/", COMPUTERS_PATH, SETTINGS_PATH],
      pathname,
    );
  }
  assert.deepEqual(navLinks("/"), [
    { href: COMPUTERS_PATH, label: "Computers" },
    { href: SETTINGS_PATH, label: "Notifications" },
  ]);
});

test("the routes the page spells are the ones the server serves", () => {
  assert.equal(SETTINGS_PATH, "/settings");
  assert.equal(SUBSCRIBE_PATH, "/api/push/subscribe");
  // The Computers screen is the fourth, and it is a path the fallback answers with the app shell like the rest
  // (ADR-0007) — a route nothing registered, so the page's own path is all that names it.
  assert.equal(COMPUTERS_PATH, "/computers");
  assert.equal(SCREENS.length, 3);
  // No two screens share a path: two links to one page is one link too many, and `navLinks` filters by value.
  assert.equal(new Set(SCREENS.map((screen) => screen.href)).size, SCREENS.length);
});

// ---- the files -------------------------------------------------------------------------------------
//
// The manifest and the icons are the acceptance's "valid installable PWA", and no shell `grep` can measure a
// shape. These read the **source** files under `web/public` (which Vite copies verbatim to `web/dist`), so the
// gate's `case_pwa` only has to prove the served bytes are these bytes.

const PUBLIC = new URL("../public/", import.meta.url);

function readPublic(path: string): Buffer {
  return readFileSync(new URL(path, PUBLIC));
}

type Icon = { src: string; sizes: string; type: string; purpose?: string };
type Manifest = {
  name: string;
  short_name: string;
  start_url: string;
  scope: string;
  display: string;
  theme_color: string;
  background_color: string;
  icons: Icon[];
};

test("the manifest is JSON with the fields an install needs", () => {
  const manifest = JSON.parse(readPublic("manifest.webmanifest").toString("utf8")) as Manifest;
  assert.equal(manifest.name, "Holosmith");
  assert.equal(manifest.short_name, "Holosmith");
  // `start_url` and `scope` are the root: the server answers the shell for every non-`/api` path (ADR-0007), and
  // a scope narrower than `/` would make `/sessions/<id>` an out-of-scope navigation.
  assert.equal(manifest.start_url, "/");
  assert.equal(manifest.scope, "/");
  assert.equal(manifest.display, "standalone");
  assert.match(manifest.theme_color, /^#[0-9a-f]{6}$/);
  assert.match(manifest.background_color, /^#[0-9a-f]{6}$/);
});

test("the manifest names a 192 and a 512 icon for any mask, and one maskable", () => {
  const manifest = JSON.parse(readPublic("manifest.webmanifest").toString("utf8")) as Manifest;
  const sizes = manifest.icons.map((icon) => icon.sizes);
  assert.ok(sizes.includes("192x192"), sizes.join(","));
  assert.ok(sizes.includes("512x512"), sizes.join(","));
  // Chrome will not offer an install prompt without both of those (or one SVG), and Android's adaptive mask
  // needs a `maskable` entry or the icon is cropped into a circle by guesswork.
  assert.ok(
    manifest.icons.some((icon) => icon.purpose === "maskable"),
    "no maskable icon",
  );
  assert.ok(
    manifest.icons.some((icon) => (icon.purpose ?? "any") === "any" && icon.sizes === "512x512"),
    "no 512 `any` icon",
  );
  for (const icon of manifest.icons) {
    assert.equal(icon.type, "image/png", icon.src);
    assert.match(icon.src, /^\/icons\/[a-z0-9-]+\.png$/, icon.src);
  }
});

test("every icon the manifest names is a PNG of the size it claims", () => {
  const manifest = JSON.parse(readPublic("manifest.webmanifest").toString("utf8")) as Manifest;
  // The manifest's list, and the apple-touch icon the shell links instead (iOS does not read `icons` for the
  // home screen when the link is present, and it will not take an SVG at all).
  const paths = [
    ...manifest.icons.map((icon) => ({ path: `icons/${icon.src.split("/").pop()}`, sizes: icon.sizes })),
    { path: "icons/apple-touch-icon-180.png", sizes: "180x180" },
  ];
  for (const { path, sizes } of paths) {
    const bytes = readPublic(path);
    // The eight-byte signature, and then the IHDR: length, "IHDR", width, height, bit depth, colour type.
    assert.equal(bytes.subarray(0, 8).toString("latin1"), "\x89PNG\r\n\x1a\n", `${path} is not a PNG`);
    assert.equal(bytes.subarray(12, 16).toString("latin1"), "IHDR", `${path} has no IHDR first`);
    const width = bytes.readUInt32BE(16);
    const height = bytes.readUInt32BE(20);
    assert.equal(`${width}x${height}`, sizes, `${path} is ${width}x${height}, not ${sizes}`);
    assert.equal(bytes.readUInt8(24), 8, `${path} is not 8 bits per channel`);
    // 6 is truecolour with alpha: a rounded icon needs a transparent corner, and a maskable one needs an opaque
    // edge — one format carries both.
    assert.equal(bytes.readUInt8(25), 6, `${path} is not RGBA`);
    // A PNG ends with IEND; a truncated file would still have a valid IHDR.
    assert.equal(bytes.subarray(-8, -4).toString("latin1"), "IEND", `${path} is truncated`);
  }
});

test("the shell links the manifest and an apple-touch icon", () => {
  const shell = readFileSync(new URL("../index.html", import.meta.url), "utf8");
  assert.match(shell, /<link rel="manifest" href="\/manifest\.webmanifest"/);
  assert.match(shell, /<link rel="apple-touch-icon" href="\/icons\/apple-touch-icon-180\.png"/);
  // iOS opens a home-screen app without Safari's chrome only when this is declared.
  assert.match(shell, /name="apple-mobile-web-app-capable" content="yes"/);
});

// ---- the two shapes a tap needs, and the route a machine's key is stored on (HS-074) ----------------

test("the notification's data carries the host with the id, so a tap can name a machine", () => {
  // The `push` handler hangs this on the notification and `notificationclick` reads it back; the two halves were
  // written apart once and the host was lost, so this is one function and one test.
  assert.deepEqual(
    notificationData({ host: "workshop", sessionId: ID, kind: "turn_finished" }),
    { host: "workshop", session_id: ID },
  );
  // Exactly two keys: a notification's `data` is this worker's own, and a third field would travel to the tap handler
  // with no reader.
  assert.deepEqual(
    Object.keys(notificationData({ host: "workshop", sessionId: ID, kind: "error" })).sort(),
    ["host", "session_id"],
  );
  // And the pair really names the machine's page — the failure the P0 produced was `""` here.
  const data = notificationData({ host: "workshop", sessionId: ID, kind: "error" });
  assert.equal(notificationPath(data.host, data.session_id), `/hosts/workshop/sessions/${ID}`);
});

test("a subscription is stored on the machine whose key signs for it", () => {
  assert.equal(subscriptionRoute("local"), SUBSCRIBE_PATH);
  assert.equal(subscriptionRoute(""), SUBSCRIBE_PATH);
  assert.equal(subscriptionRoute("workshop"), "/api/hosts/workshop/push/subscribe");
  // The same path `transcript.ts` builds for the hop — the second spelling, made safe here.
  for (const host of ["local", "workshop", "build-box-2"]) {
    assert.equal(subscriptionRoute(host), apiPath(host, "push/subscribe"), host);
  }
});

test("a row says a word only once its machine has answered", () => {
  // `null` is "nothing has been read", which is why the page draws the name and `reading…` rather than a claim.
  assert.equal(machineSubscription("subscribe"), "not-subscribed");
  assert.equal(machineSubscription("blocked"), "not-subscribed");
  assert.equal(machineSubscription("no-key"), "not-subscribed");
  assert.equal(machineSubscription("unsupported"), "not-subscribed");
  // Only the arm that would undo a subscription says one exists.
  assert.equal(machineSubscription("unsubscribe"), "subscribed");
  // And a machine that did not answer is the reach word, never `not-subscribed`.
  assert.equal(machineSubscription("away"), "unreachable");
  // Every arm is covered: a seventh word would have to be classified here.
  const actions = ["subscribe", "unsubscribe", "blocked", "unsupported", "no-key", "away"] as const;
  assert.equal(new Set(actions.map(machineSubscription)).size, 3);
});

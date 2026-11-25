# A promotion is on by default, and `[update] enabled` is a kill switch

`[update] enabled` was `false` by default, and `true` was refused unless `[update] source` named a checkout. The reason
was written on both keys: a promotion runs the gate and then restarts this machine's own service, and an upgrade must
not hand a phone a restart button by accident. The cost was two keys on every machine that wants the button, and a
cross-key rule that turns a machine with no checkout into a file that is **wrong** rather than a machine with nothing
to promote.

That reason is reversed here, because it was a check on the *release* rather than on the operator. The release that
carries `POST /api/update` is one the operator deployed themselves, by a command they ran; the authority the route
spends is the one the route already has, which ADR-0110 D1 decided is `tailscale serve` and the tailnet's ACLs. A
second switch defaulting to off is therefore a boundary nobody asked for, and the thing a phone sees after an upgrade
is a button that says "this machine has not opted in" — one more key to write before the decision D1 already took can
be used.

## D1 — `true` is the default, and `false` is a kill switch

`config::Update`'s `enabled` field defaults to `true`, so a machine answers `POST /api/update` and
`GET /api/update/candidate` unless its own file writes `false`. The key keeps its name and its wire meaning: `false`
still answers `403 update_disabled`, and the sentence still names the key.

What changes is what the sentence means. It used to describe the state an upgrade leaves behind; now it describes a
machine whose operator turned the route off, so `ApiError::update_disabled` offers **removing the line** beside setting
it to `true` — a machine that never wanted the button should not have to keep a `false` around to say so.

**The role still refuses first.** A `hub` answers `403` with its own sentence whatever this key says, because a hub has
no release tree and no Docker for the gate's container — that is a fact about what a hub *is*, not a preference its
file may set.

## D2 — `enabled` and `source` are two questions, so the cross-key rule is gone

`update_enabled_problem` refused `enabled = true` with no `[update] source`, on the ground that it was a machine saying
"a promotion may be started" and unable to say what would be promoted. With the default flipped that rule would refuse
**every** config that has no `[update]` section, which is most of them, and it was answering the wrong question anyway:

- an unset `source` is `candidate::Refusal::NoSource` — `404 no_candidate`, whose sentence names `[update] source` and
  the command that repairs each of the five shapes;
- `enabled = false` is `403 update_disabled`.

Those are two answers to two questions, and only the second is about the file's own choice. A machine with no checkout
is not contradictory; it has nothing to promote, and the candidate route says so to a reader who can act on it — which
is a better answer than a config refusal that stops `serve` from booting at all.

The rule is therefore removed rather than inverted, and `[update] source` alone is now the whole of what a promotable
machine writes.

## D3 — What the rendered config, the sentence and the tests say

`Config::default().update.enabled` is `true`, so `config show` and the golden file (`tests/golden/config-default.toml`)
render `enabled = true`. A deploy renders the config it **loaded**, so a machine that has written nothing gains the one
line at its next deploy and no other key — and a machine that wrote `false` keeps it, which is the kill switch working.

The arms that measure the refusal stay, and stay honest: they now write `enabled = false` themselves, and each asserts
the default is on so that a flip back cannot pass as the operator's choice. The candidate route's `404 no_candidate` arm
names no switch at all, which is what makes it the arm that measures the default over a socket.

## Rejected

- **Keeping `false`.** The upgrade that carries the route is the operator's own deploy. A release cannot mail a phone a
  button the way the old comment feared: it arrives because the operator ran `deploy` or pressed promote on a machine
  that already had this authority.
- **`enabled` defaulting to `source.is_some()`.** It couples one key's default to another's value, so `config show` and
  a deploy's render would print a value no file wrote, and a machine that later points `source` at a checkout would
  silently gain the route. D2 removes the coupling, so there is no such state left to express.
- **Removing the key.** An operator who wants a machine nobody may restart from a browser still needs a way to say so,
  and deleting `[update] source` would be a different claim: "this machine has nothing to promote" rather than "it will
  refuse".

## Consequences

- Every existing config with no `[update]` section answers the promotion routes: `GET /api/update/candidate` moves from
  `403 update_disabled` to `404 no_candidate`, and a `POST` follows it. The docs' `[update] enabled` row and the
  cross-key rule list say so; the golden file carries `enabled = true`.
- ADR-0110 D1's recorded consequence is now reachable without a second act: a hub page can promote any registered host,
  and the tailnet's ACLs are the whole of what stands in the way. That was already true of every machine that wrote
  `enabled = true`; it is now the default.
- `docs/config.md` loses the bullet that said the two keys were cross-checked, because they are not.

## What is not measured

- **A promotion pressed on a machine that never wrote the key.** It needs a green gate, a daemon and a release tree,
  which is the parent's gate run; the route's answers over a socket are measured in `tests/update.rs`, whose candidate
  arms now start from the default.
- **A config file from an older release.** The key's meaning moved and its default moved; nothing migrates, because a
  file that wrote `enabled = false` keeps refusing and a file that wrote nothing gains the route. No gate case promotes
  across an upgrade to measure that, and the shapes are the two the unit tests pin.

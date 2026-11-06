# The key proxy's request is its bytes, and its store is a directory

HS-040 is a verification story: V3, V4 and V5 are the checks that say spec item 2's credential path actually
works. Running them against a real provider key found two defects in HS-039's proxy. Both are recorded here,
because each one is a shape a later reader would otherwise restore: a `Content-Length` with no body, and a file
bind mount over a file that is written by rename.

## The forwarded request is head **and** body

`keyproxy::upstream` built a request whose `Content-Length` came from `body.len()` and whose bytes stopped at
the blank line. Every model call was therefore a request the provider waited out and closed — the proxy answered
`502 the provider closed the connection before answering`, about sixteen seconds later, after the provider's own
idle timeout. The unit tests could not see it: they asserted the header, and a header is exactly what a
bodyless request still has.

The function now returns `Vec<u8>` — the head with the body appended — and its doc says that the body is part of
the request rather than an argument to its length. `the_upstream_request_is_the_head_and_then_the_body` pins
`ends_with(body)` and "exactly one `Content-Length`", which are the two things a head-only request cannot satisfy.

The general rule: **a frame that declares a length is a claim about bytes that must be in the same value.** A
test for such a frame asserts the bytes, not the declaration.

## The key store is mounted as a directory, because the store is written by rename

`<data>/secrets/keys.toml` is written by temp-file-and-rename (ADR-0048), and that is not negotiable: the one
file a real key lives in must never be observable half-written. A **file** bind mount pins the inode, so a
container started before the first create went on reading the file as it was at mount time. `hs_key` starts at
boot and every session's dummy is written by a later rename, so *no session* could ever authenticate: the proxy
answered `401 … session=- provider=opencode-go status=bad-key` for a dummy key it had minted itself.

`KeyProxy::mount` is therefore `keys`'s parent, and the run vector mounts `<data>/secrets` read-only at its own
host path while `--keys` still names the file inside it. A directory mount resolves the name at read time, so a
rename inside it is visible. `the_key_proxy_mounts_the_binary_and_the_directory_the_key_file_lives_in` asserts
both halves — the directory is a source, the file is **not** — and the gate's `case_internet_toggle` reads the
mount list off the running `hs_key` and now fails on a file source.

The general rule: **a container that reads a file this build replaces by rename must mount the directory**, and
the same applies to any future reader of a file written that way (the WIP snapshot's refs and the bundle's
`.tmp`-then-rename are the other two).

## A provider row is one (host, path, auth) triple — and one API shape

V3/V4/V5 need a provider this machine has a key for. `opencode-go` (opencode.ai's Go gateway) is that provider,
and it serves two shapes: `anthropic-messages` under `/inference/go/anthropic` and `openai-completions` under
`/inference/go/openai/v1`. pi appends `/v1/messages` or `/chat/completions` to the `baseUrl` it is given, so a
single row can serve exactly one of them — the shape is part of the row's identity, not a per-request choice,
because a per-request choice is a request-controlled path.

The row added here is the anthropic shape (`/inference/go/anthropic`, `x-api-key`), because that is the shape of
the model this story verified (`claude-haiku-5-5`, which streams thinking deltas at `--thinking max`). A second
row for the openai shape would be `opencode-go-openai` with `/inference/go/openai/v1` and `Bearer`; nothing in
the table needs to change for it, and `models.json` would then need the `models` array V4's fallback names. The
table stays exact — a missing provider is a refusal that names what is supported.

## What the three checks measured

- **V3 passes.** A real session (`opencode-go/claude-haiku-5-5`, `thinking = "max"`) answers with text deltas, a
  `bash` tool round trip, and `usage` on `message.end`; its container holds only the dummy key.
- **V4 passes.** `get_available_models` inside the session's own container, with `PI_OFFLINE=1` and only the
  dummy key in `models.json`, lists 29 models for the provider from pi's bundled catalog — no network, no
  catalog fetch.
- **V5 passes with its documented fallback.** Precedence is `auth.json` > `models.json` `apiKey` > the provider
  environment variable, measured against the proxy's own log: a competing `auth.json` reaches the proxy as
  `status=bad-key`, and a competing `OPENCODE_API_KEY` does not displace the `models.json` dummy. So the check's
  pass condition holds only while the seeded pi-home carries no `auth.json` and the container environment
  carries no provider key — which is what HS-039 already does, and the fallback sentence ("remove the other
  sources from the seeded pi-home and the container environment") is the rule that keeps it true.

V3's failure fallback (a per-session spend-limited key in the environment) is not needed and was not taken; the
dummy key never leaves `models.json`, and the environment variable that *would* carry a spend-limited key is
measurably the weakest of the three sources anyway.

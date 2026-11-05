# The key proxy is a second plaintext proxy, and its allow-list is a provider table

Spec item 2's credential half has one sentence with teeth: a real provider key never enters a container. HS-037
built the network's half — a session's network is `--internal`, and the egress proxy is the only route out.
This story builds the other half: pi talks to `http://key-proxy:8080/<provider>` with a per-session **dummy**
key, and one container on the `egress` network holds the real key and does the TLS. The decisions below are the
ones a later reader would otherwise undo.

## The store is one file, and the dummy keys live in it too

`<data>/secrets/keys.toml` (mode 0600) is the only file a real key is written to. It has two tables:

```toml
[providers]
anthropic = "sk-ant-…"

[sessions]
0123456789abcdef = "hs-dummy-…"
```

The session table is not a convenience. `hs_key` is a container with no database, no mount of `app.db` and no
RPC channel to the server; the only way it can answer "is this dummy key still valid?" is to read the same file
that minted it. The alternative — a second store, or an in-memory map in the server and an HTTP call to it —
would add a second source of truth to a file whose whole job is being the one place a secret lives.

Writes are **temporary file + rename**, and the temporary is opened with mode 0600 before the rename. A proxy
reading at the wrong moment sees the old file or the new one, never a half-written key; a crash leaves the old
file. `key set` reads the value from stdin only: an argument is a key in `ps`, in shell history and in a crash
report, and the spec names stdin for that reason.

The store's parser is deliberately the two tables this file writes, not a general TOML reader. The repository's
config layer already owns the `toml_edit` dependency and a guard says only `src/config/` may name it; a second
TOML grammar in a key module would be a second spelling as well as a second dependency. A key that is not a
quoted string is refused, not guessed at.

## The provider is the path's first segment, and the host is a table

A request arrives as `/anthropic/v1/messages` or `/openai/chat/completions`. The first segment selects a row in
`key::PROVIDERS`; the row fixes the host, the base path and the header the real key goes in. There is no
request-controlled host or port. This is ADR-0045's exact-list rule applied to the other proxy: an agent that
can spend its dummy key must not also be able to say where the proxy connects, or the key proxy becomes a
tunnel to the control listener.

The table is small and explicit (`anthropic`, `openai`, `openrouter`, and a handful of OpenAI-compatible
providers). An unknown provider is a 403 with a sentence naming the supported ids. Adding a provider is one row,
which is the right size for a fact that is a host and a header.

## TLS is `openssl s_client`; the artifact is static

The artifact has no TLS stack by design (ADR-0005's static musl build), and the key proxy has to terminate
plaintext and speak TLS to a provider. The agent image carries `/usr/bin/openssl` (measured), so the proxy runs
`openssl s_client -quiet -connect <provider>:443 -verify_return_error -verify_hostname <provider>` as a child and
streams bytes through its pipes. The alternative — adding `rustls` to the binary — is a real dependency and a
second TLS implementation to keep patched; the alternative at the other end — leaving the key in an environment
variable so a plaintext forward proxy can add it — is what the story forbids.

Three consequences are recorded rather than hidden:

- **A missing `openssl` is a 502**, not a silent fallback. A machine whose image lacks it cannot use the key
  proxy, and the sentence says so.
- **The request is held in memory** up to `MAX_BODY` (32 MiB) because `Content-Length` has to be written for the
  upstream request; the response is never held, it is copied chunk by chunk. That is the streaming half: an SSE
  body is forwarded as it arrives, not buffered behind a length that does not exist.
- **`Connection: close` is sent upstream**, so a response has an end. A provider that ignores it would hold a
  key-proxy task open; the alternative is a response parser, and the proxy does not need one.

## The dummy key is minted by the server and revoked by the delete

`Store::dummy_for(session)` mints `hs-dummy-<32 random bytes>` on first use and returns the same value on a
resume or a restart. `DockerBackend::start_container` writes `models.json` from the current store on every
start: it contains the key proxy's URL, the dummy key and nothing else. `settings.json` keeps HS-031's
seed-only-if-absent rule, because pi writes its own state there; `models.json` is this build's file and is
rewritten so a resumed session sees a key that is still in the store.

The delete flow's `revoke_key` is no longer a no-op: it removes the session's row from `[sessions]` **before**
the backend tears anything down, which is the same order the no-op documented. A dummy whose session is gone is
refused with 401 by the proxy even if a container from that session is still alive for a moment.

## The two proxies are joined by the network step, and one of them is unconditional

`hs_egress` is joined only when the row says `network = true` (ADR-0046). `hs_key` is joined to **every** session
network, because the model provider is reachable with the checkbox either way. It is joined by the same network
step, under the alias `key-proxy`, and it leaves before `network rm` for ADR-0045's reason (`docker network rm`
refuses a live endpoint). A failure is a `warn`, not a refusal: a missing key proxy fails a model call, not a
session.

The self-test gains the inverse of ADR-0046's pair: after the bare probe it joins `hs_key` and asks for
`key-proxy:8080` **alone**, requiring an answer. That is not a verdict about isolation — a missing key proxy is
HS-039's `warn` — but it is the only boot-time measurement that the one legitimate address on an off network
actually resolves.

## What the network does and does not enforce

`hs_key` publishes no port and is attached only to `egress` and session networks. A request from the wrong
network therefore has no route to it; a request from a session network still needs a dummy key that is in the
file. The ADR does not claim the proxy can inspect a provider's TLS content — it cannot; that is the same
accepted risk ADR-0045 records for egress. It does claim the two things the story can prove without a provider
key: no dummy is a 401, and a dummy for a provider with no stored key is a 503, both before any TLS client is
started.

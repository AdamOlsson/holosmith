# The proxy is a mode of this binary, its rule is exact, and the address decides

Spec item 2 moves enforcement from an environment variable to the network: a session's network is
`--internal`, so nothing leaves it, and the only thing attached to it that has a route out is a proxy this
build starts and controls. HS-036 measured the first half. HS-037 is the second: `src/egress.rs`, the
`holosmith proxy egress` subcommand, and the container that runs it (`hs_egress`, item 10 §3's deploy step
names it). This records the five decisions that were not forced, and the one gap it deliberately leaves to
HS-038.

## The proxy is a second mode of the binary, and the container runs *this* image

The spec's own list of internal subcommands — "used inside containers" — is `holosmith proxy egress`,
`holosmith proxy key`, `holosmith container snapshot`, `holosmith container fake-pi`. So the proxy is a
`clap` subcommand, not a second program: one artifact to build, one to ship, and nothing that can drift out of
step with the server it belongs to.

Which image runs it is a smaller question than it looks, because the artifact is **static musl** (that is what
`check-static.sh` asserts): any Linux base can execute it. The proxy container is therefore
`docker run --detach --name hs_egress --network egress --restart unless-stopped --cap-drop ALL
--security-opt no-new-privileges -v <binary>:<binary>:ro --entrypoint <binary> <agent_image> proxy egress
--listen 0.0.0.0:3128 --allow …`, where `<binary>` is **this process's own executable**
(`std::env::current_exe()`), mounted read-only at its own host path — item 1's identity rule, applied to a
container that mounts nothing else.

Three consequences, recorded rather than hidden:

- **The deploy step's `holosmith-proxy:<version>` becomes optional rather than required.** When HS-048 builds
  that image, the mount disappears and the image changes; that is one field on [`EgressProxy`], which is why
  `EgressProxy::of` takes the machine's `Config` and not a `ContainerSettings`.
- **The proxy is replaced on every boot** (`rm -f`, then `run`). The allow-list is an argument, so a changed
  `[egress] allow` takes effect at the next `serve` without an operator touching anything.
- **`--restart unless-stopped` and `--rm` conflict**, so a stopped proxy keeps its name. That is the same
  reason spec item 6's `rm -f` before every start is load-bearing for a session's container.

## The allow-list is an exact domain, and a wildcard stays refused

`config::domain_problem` already refuses `*` in `[egress] allow` — "a bare domain with no scheme, path, port
or wildcard". A **suffix** match (`host.ends_with(entry)`) would be that wildcard under another name: it
permits every subdomain of a name an operator wrote, including ones that are not the registry's. So
`Rules::permits` compares the whole name, lowercased with one trailing root dot dropped (DNS is
case-insensitive and `crates.io.` is the same domain). A registry that needs `crates.io` **and**
`static.crates.io` lists both, and the ADR is where an operator reads that.

The alternative — a config file that permits `*.crates.io` — is a change to what `config validate` refuses,
not a proxy detail, and no story has asked for it.

## The address is checked before the connection, and every answer is checked

Item 2's fourth bullet is the one with teeth: a destination that resolves into the machine is refused
**regardless of the allow-list**. `address_problem` names the range in the operator's words — loopback
(127/8, `::1`), link-local (169.254/16, fe80::/10), RFC 1918 (10/8, 172.16/12, 192.168/16), 100.64.0.0/10,
plus the unspecified address and IPv6's unique-local range, which are the same fact in another family — and
it judges an IPv4-mapped IPv6 address as the IPv4 address it carries, because otherwise the check is one
spelling deep.

Two properties follow, and both are tested:

- **Any** answer refuses the name, not just the first. A dual-answer host (one public, one loopback) must not
  be usable through the answer the check did not look at.
- `localhost` **in the allow-list** still gets 403. That is the case that passes if the address rule is
  skipped for listed names, and it is the gate's case rather than only a unit test.

The measurement this rests on is ADR-0044's: from inside an `--internal` network its *own* gateway answers
(`refused`), so a container can reach whatever the host binds to `0.0.0.0`. A proxy that tunnelled to
`172.21.0.1` would hand a session the control listener; refusing private ranges is the proxy's half of a gap
the network cannot close.

## Port 443 only, `CONNECT` only, and the status says which

A tunnel is the only thing this proxy does: it never reads a request, so it can never be confused into
forwarding one (`CONNECT host:80` is 403, not a plaintext proxy request). Anything that is not `CONNECT`
is **405**, because the honest answer to `GET http://…` is "this proxy does not do that" and not "that host
is not allowed". A port other than 443 is **403**, before the name is resolved — the policy decides as much
as it can from values, and resolution is the only I/O in the decision path.

## Attribution is the proxy username, and the environment variable carries it

Item 2 wants "host and bytes per request, using the session id as proxy username for attribution", and the
acceptance says the containers get `HTTPS_PROXY=http://egress-proxy:3128`. Both are true only if the session
id travels in the URL's **userinfo**, so that is what the container gets:
`HTTPS_PROXY=http://<session-id>@egress-proxy:3128`. The proxy reads the username from
`Proxy-Authorization: Basic …`, validates it with `tool::valid_id` (an unvalidated string reaches a log line),
and logs `session=… host=… status=… bytes_up=… bytes_down=…` — one line per answered request, at `info`,
which is what `scripts/verify.sh` greps. A request with no credentials is logged as `-`, not dropped: the
refusal is the interesting part.

`egress-proxy` is a **network alias**, not the container's name: the proxy is joined to a session's network
with `docker network connect --alias egress-proxy`, and Docker's embedded DNS answers a container's name and
its aliases only on networks the asker shares with it. That is also "resolve nothing externally" — the
session's resolver knows the network's own endpoints and nothing else.

**The password is empty on purpose.** Nothing checks it, and item 2 forbids a secret in an environment
variable; the *identity* is what the proxy needs, and the session id is not a secret.

## The attach is the network step's, and the checkbox decides it

The proxy is joined to a session's network by the network step (`provision_network`, and `resume`, and left
before `remove_network` because `docker network rm` refuses a network with an active endpoint). It is not a
fifth `Step`, because a `Step` is a value inside the `session.status` event a client reads: "the session's
network" is one step, and its report already means "the session has a network".

**It runs only when the session's row says `network`.** Item 2 makes the checkbox topology rather than
configuration, so a session created with it off is a network the proxy is not joined to — the same
`HTTPS_PROXY` in the container either way, and nothing answering on it. Leaving that unconditional for one
story would ship a machine whose checkbox is ignored, which is the kind of gap a later story quietly blesses;
so HS-037 honours the flag even though **HS-038** owns the UI, the off-case self-test and the demo.

**A failure is a `warn`, never a refusal**, and the reason is item 2's own framing: an unreachable proxy fails
a package download, not a session. It is also what keeps a machine whose proxy image is not built yet able to
serve — and the control UI is the one screen an operator needs to read when something is wrong with the
proxies.

## What is left, on purpose

- The provider hosts are **not** on the allow-list, because the key proxy is what makes a model call work
  (HS-039). `NO_PROXY=key-proxy` is already in the launcher's environment for that reason.
- HTTPS is opaque to a `CONNECT` tunnel, so an allow-listed host is an exfiltration channel. Item 2 lists that
  as an accepted risk; this ADR repeats it because the log line's *bytes* are the only thing that can be said
  about a request's content.
- An orphaned `hs_egress` after a crash is HS-044's reconcile, and a proxy that is not running is one `warn`
  at boot.

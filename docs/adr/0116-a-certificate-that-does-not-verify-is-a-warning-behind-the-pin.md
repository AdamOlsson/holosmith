# A certificate that does not verify is a warning, and the pin is what decides

`musl.cc`'s TLS certificate expired at 2026-11-26 01:56:26 UTC and was not renewed. Both recipes fetch their two musl
cross toolchains from that host at image-build time over `https://` (`crates/holosmith/images/agent.Dockerfile`,
`builder.Dockerfile`), `curl` validates the chain against the image's `ca-certificates`, and the fetch ends at

```text
 > [stage-1 7/17] RUN curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz https://musl.cc/…:
curl: (60) SSL certificate problem: certificate has expired
```

Measured on this host: `holosmith-proxy:0.1.1` and `holosmith-base:0.1.1` build, `agent-image:0.1.1` stops there, and
`scripts/verify.sh` records `FAIL case_server_images/deploy/exit`. The host is healthy — `http://musl.cc/…` answers
`200`, and `curl -k https://musl.cc/x86_64-linux-musl-cross.tgz` answers `200` with `Content-Length` 115063639. Only
the certificate is broken.

**Why this is a defect rather than a wait.** The tarballs are already pinned by SHA-256 **in the same `RUN`**
(`| sha256sum -c -`), and that pin is the integrity guarantee: the bytes that arrive are the bytes that were vetted,
whoever served them. The certificate contributes transport authentication — that the answer came from `musl.cc` — and
nothing about what lands in `/opt`. ADR-0111 already says a vendored copy or a mirror is *technically safe to
substitute* for exactly this reason. So today a third party's lapsed certificate blocks every release build on every
machine: `server deploy`'s step 3, `holosmith update`'s gate, `holosmith build`'s first run, and any `upgrade` whose
labels say the recipe changed — for a check the pin already subsumes.

## D1 — The fallback is guarded by curl's certificate statuses, and by nothing else

Each of the four fetch instructions keeps its verified attempt exactly as it was — `curl -fsSL --retry 4
--retry-delay 2 --retry-all-errors` (ADR-0111 D1) — and wraps it:

```sh
if curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o /tmp/musl.tgz <url>; then :; else
  status=$?
  case $status in 51|60|77|83) echo "warning: musl.cc's certificate did not verify (curl exit $status): retrying that URL without certificate verification, and the pinned digest decides" >&2 ;;
  *) exit $status ;; esac
  curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors --insecure -o /tmp/musl.tgz <url>
fi
```

and the `| sha256sum -c -`, `tar` and `rm` that followed stay where they were, in the same `RUN`.

Curl's `51` (peer certificate or fingerprint not OK), `60` (the certificate could not be verified), `77` (its CA
certificate) and `83` (an issuer check failed) are the four statuses that say *the certificate* is what failed. Every
other status stays fatal and unchanged, with the child's own code: `6` the name that did not resolve, `7` the refused
connection, `28` the timeout, `22` with `returned error: 5` for a host declining, and a digest that did not match. So
ADR-0111's classification is untouched in both directions — a host that stays away is still a failed build whose
sentence says so, `images::fetch_did_not_answer` still reads the build's last status, and `server deploy` still answers
4 for it.

**`/bin/sh` is dash, so the fallback is `case` on a captured `$?`** — an `if`/`else`/`case`/`esac` and `exit $status`,
no bashism anywhere, because `echo -e`, `[[` and `${status}`'s arithmetic neighbours are all things dash either lacks
or spells differently. The failing redirect is the arm's own `exit`; the failing fallback is the `if`'s status, which
the `&&` chain after `fi` reads, so a fallback that does not deliver fails the build exactly as the single fetch did.

## D2 — The pin decides what lands, and the certificate decides who served it

The two digests are unchanged, and they are still checked after the fallback rather than instead of it. A certificate
that could not be verified widens *who* could have answered the request to anyone able to serve those exact bytes; it
does not widen *what* reaches `/opt`, because the digest refuses everything else. That is the whole argument for the
fallback, and it is why the fallback is not extended to the statuses above: those are a fetch that did not arrive, and
retrying one without verification would not make it arrive.

## D3 — The warning is surfaced, not swallowed

A silent fallback is the shape this repository does not do, so the recipe's line is a fact an operator hears:

- Both recipes print one line on stderr whose first words are `images::CERTIFICATE_WARNING`'s marker — the constant
  exists so the recipes, the reader and a grep cannot drift apart.
- `images::certificate_warnings` reads those lines out of a build's log, **only from a successful build**: a failed
  build's sentence is the whole of what it says, and the warning is not what went wrong. The marker has to *begin* the
  line, after BuildKit's `#8 0.512 ` prefix: BuildKit also echoes the instruction it failed on (`#8 [4/9] RUN …`,
  ` > [4/9] RUN …`, `  76 | >>> RUN …`) and the shell in its last `did not complete successfully` line, and every one
  of those carries the whole `RUN`, marker and all. A text search over the log would read the echoes back as warnings.
- `images::ensure` replays each one through the `progress: &mut dyn Progress` sink it already has, beside the
  `building <tag>` and `built <tag>` notes, so it arrives in the step's own label: `install: [3/6 images] warning:
  musl.cc's certificate did not verify (curl exit 60): …`.
- **Both recipes have a reader, and each sentence carries the clause.** `images::ensure` builds the three product
  images, reads the agent recipe's line back out of the base and agent builds for the `install` sink, and composes
  step 3's sentence. `holosmith build` builds the builder recipe through `builder::ensure_image`, which has no sink and
  no step, so its sentence is the whole of what that command prints about the builder image —
  `build: builder: holosmith-builder:0.1.1 was built from this recipe; musl.cc's certificate did not verify, so the
  pinned tarball was fetched without verification and its digest matched` — and it is composed from the same
  `images::CERTIFICATE_CLAUSE` rather than a second spelling of the sentence.

**What each verb prints follows ADR-0092, and the sentence is where a deploy carries it.** `install` is the verb that
announces, so the sink is where the line is read; `server deploy` and a promotion pass `Silent`, because their report is
the whole of what they print. So `ensure`'s sentence carries the fact too — `built 3 images: …; musl.cc's certificate
did not verify, so the pinned tarball was fetched without verification and its digest matched` — once for the run
however many builds warned, because both tarballs print the same line. Nothing new goes to a stream: `--verbose` prints
it as step 3's sentence and the terse report prints it under the step's mark, which is the report a deploy is. The one
clause is appended **once for the whole step**, however many builds or tarballs warned: a `bool` carried beside the
sink is what the sentence's shape needs, and the sink is where the count of lines belongs.

## D4 — A warning does not weaken a classification

The warning is a fact about a fetch that ran; it is not evidence about how the build ended. A log that carries a
warning line and then fails with `exit code: 1` — a compile error, a `COPY` of a file the archive does not carry — is
still the tree's, and a tarball that arrived after the fallback and failed its digest is still the tree's for the
sharper reason that the pin is what refused it. `images::fetch_did_not_answer` reads the build's **last** status and
never a diagnostic, so the fallback adds nothing for it to read; the unit tests pin both directions.

## What is not measured

- **A valid-certificate run.** While upstream's certificate is expired, every real build takes the fallback, so the
  verified-only path is the first attempt of code that did not otherwise change and nothing exercises it end to end
  until the host renews.
- **A dash shell against the real instruction.** The `RUN` was built by hand in a scratch context over
  `node:24-bookworm-slim` (`/bin/sh` → `/usr/bin/dash`), with the wording these recipes now carry: the verified attempt
  made five attempts ~2 s apart and failed with 60, the warning line printed, the `--insecure` retry fetched the
  115 MB pinned tarball in 85 s, `sha256sum -c -` answered `OK` for the recipe's own digest, and the archive extracted.
  A second `RUN` of the same shape against a name that does not resolve exited **6** and never reached the line after
  the `if`, so the other statuses are fatal with the child's code. What the parent's gate then measures is the same
  shape inside the real `agent-image` build.
- **The clause on a real deploy.** `tests/deploy.rs` drives step 3 over a fake build log and `images.rs`' unit test
  drives `ensure` the same way, so the sentence is measured; `builder.rs`' unit test drives the builder image's the same
  way. A real deploy's clause depends on the building host's certificate, which no case may assume, so the gate's image
  case asserts the `^3\. images: changed: ` prefix and not what follows it.
- **Every other way a certificate can fail to verify.** An expired chain, a wrong hostname and an untrusted issuer all
  arrive as one of the four statuses; `51` and `83` are curl's own and were not reproduced here.

## Consequences

- A from-scratch build of `agent-image` and `holosmith-builder` completes while that host's certificate is broken, and
  `scripts/verify.sh`'s image cases measure the tree again instead of a certificate.
- Exit codes are unchanged: 4 is still a fetch that did not answer (ADR-0111), 1 is everything else, and a digest
  mismatch after a fallback is still a refusal of the tree.
- `AGENTS.md`'s musl cross-toolchain bullet and the two recipes' comments say what the pin covers and what the
  certificate buys, which is what a reader looking at a failed fetch needs first.
- `tests/images.rs` asserts the shape of both recipes — the four statuses guard the fallback, `--insecure` appears
  exactly once per fetch and after the verified attempt, `sha256sum -c -` still follows it, and the marker is printed —
  so a later edit cannot drop a half of it silently.

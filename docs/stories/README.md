# Holosmith — implementation stories

The full `HolosmithSpec.md` broken into 80 thin vertical slices, ordered so that something runs on day one and every story after it ends in a state you can demo.

- [Phase 0 — Walking skeleton](./phase-0-walking-skeleton.md) · HS-001 – HS-006
- [Phase 1 — Session lifecycle against the stub backend](./phase-1-session-lifecycle-stub.md) · HS-007 – HS-023
- [Phase 1b — The real Docker backend](./phase-1b-docker-backend.md) · HS-024 – HS-046
- [Phase 2 — Deployable MVP](./phase-2-deployable-mvp.md) · HS-047 – HS-067
- [Phase 3 — Multiple machines](./phase-3-multiple-machines.md) · HS-068 – HS-074
- [Phase 4 — Self-development](./phase-4-self-development.md) · HS-075 – HS-080

Vocabulary lives in [GLOSSARY.md](../../GLOSSARY.md). Decisions taken while slicing live in [docs/adr](../adr/).

## What a story is

Each story is a vertical slice: it touches every layer needed to produce an observable outcome, and it ends with something you can show. A story is **not** "write the git layer" or "add the events table" on its own.

Every story carries:

| Field | Meaning |
| --- | --- |
| **As / I want / so that** | The user-visible value. The actor is the operator, the agent, or the maintainer. |
| **Size** | `XS` ≤ 2h · `S` ½–1 day · `M` 1–2 days · `L` 2–4 days. Anything larger was split. |
| **Blocked by** | Story ids that must be closed first. No other ordering is implied. |
| **Spec** | Which spec items the story implements. This is how coverage is audited. |
| **Demo** | The one thing you show when it is done. If you cannot show it, the story is not done. |
| **Acceptance** | Checkboxes, each independently verifiable. Spec numbers are quoted, not paraphrased. |

**Definition of done:** acceptance criteria met, `scripts/verify.sh` extended with at least one case and green, the demo performed, and the spec updated if the story changed a decision.

## Sequencing

The dependency chain is nearly linear by design; where it branches, the branches can run in parallel on separate worktrees.

```
HS-001 build ─ HS-002 config ─ HS-003 serve ─ HS-004 db ─ HS-005 guards ─ HS-006 verify
                                                                              │
HS-007 sessions ─ HS-008 create ─ HS-009 events ─ HS-010 stream ─ HS-011 stub ─ HS-012 states
                                                                              │
                        HS-013 list ─ HS-014 transcript ─ HS-015 prompt ─ HS-016 abort ─ HS-018 stop/resume
                                                                  │                    │
                                                             HS-017 steer        HS-019 delete ─ HS-020 cap ─ HS-021 idle
                                                                                       │
                                                                                  HS-022 timeout ─ HS-023 budget
                                                                                       │
                                                              ── phase 1 exit: the product works, with no Docker ──
                                                                                       │
HS-024 backend ─ HS-025 worktree ─ HS-026 refusals ─ HS-027 mounts(V1) ─ HS-028 container ─ HS-029 names ─ HS-030 exit
                                                                                       │
                                          HS-031 pi ─ HS-032 deltas ─ HS-033 tools ─ HS-034 usage ─ HS-035 V8
                                                                                       │
                     HS-036 network ─ HS-037 egress ─ HS-038 checkbox(V6) ─ HS-039 keyproxy ─ HS-040 V3/V4/V5
                                                                                       │
                     HS-041 preview(V7) ─ HS-042 snapshot ─ HS-043 delete-keep ─ HS-044 reconcile(V9)
                                                                                       │
                                                              HS-045 doctor ─ HS-046 verification run
                                                                                       │
                                                              ── phase 2 ── HS-047 deploy ─ HS-048 images ─ HS-049 service
                                                                                       │
     HS-050 status ─ HS-051 drain ─ HS-052 changes ─ HS-053 diff ─ HS-054 ff ─ HS-055 merge/squash ─ HS-056 guards ─ HS-057 publish ─ HS-058 UI
                                                                                       │
     HS-059 coalesce ─ HS-060 compaction ─ HS-061 history ─ HS-062 disk floor ─ HS-063 disk cap ─ HS-064 ttl ─ HS-065 backup ─ HS-066 push ─ HS-067 pwa
                                                                                       │
                                                              ── phase 3 ── HS-068 registry ─ HS-069 forward ─ HS-070 trust(V11) ─ HS-071 UI ─ HS-072 hub cli ─ HS-073 ssh ─ HS-074 cross-machine
                                                                                       │
                                                              ── phase 4 ── HS-075 migrations ─ HS-076 builder ─ HS-077 smoke ─ HS-078 upgrade ─ HS-079 rollback ─ HS-080 releases
```

**If you only do the first ten:** HS-001 → HS-010. That is a running binary, guarded, with durable events and a live stream. Nothing else in this plan is more expensive to retrofit than the guards (HS-005) and the single writer (HS-009).

## Effort

Rough order of magnitude, for planning only:

| Phase | Stories | Effort |
| --- | --- | --- |
| 0 — Walking skeleton | 6 | ~5 days |
| 1 — Lifecycle on the stub | 17 | ~18 days |
| 1b — Real Docker | 23 | ~32 days |
| 2 — Deployable MVP | 21 | ~26 days |
| 3 — Multiple machines | 7 | ~12 days |
| 4 — Self-development | 6 | ~10 days |

The `L` stories (HS-031, HS-037, HS-039, HS-047, HS-069, HS-077, HS-078) are the ones that will actually take longer than estimated: each is a place where an external system's behaviour is not yet known.

## Coverage audit

Every section of `HolosmithSpec.md` maps to at least one story. If a spec item gains a requirement later, the matrix says which story is now incomplete.

| Spec item | Stories |
| --- | --- |
| 1. Worktrees and the hardened `.git` mount | HS-025, HS-026, HS-027, HS-042, HS-065 |
| 2. Egress enforcement and credentials | HS-036, HS-037, HS-038, HS-039, HS-040 |
| 3. Separate origin for previews | HS-041, HS-069 |
| 4. Browser and hub exposure | HS-005, HS-049, HS-068, HS-070 |
| 5. Durable events and batching | HS-009 |
| 6. Container naming and exit handling | HS-029, HS-030, HS-044 |
| 7. Creating a session | HS-008, HS-011, HS-044 |
| 8. State machine | HS-012, HS-018, HS-020 |
| 9. Eviction and delete | HS-019, HS-042, HS-043 |
| 10. Promotion, rollback, migrations, drain | HS-047, HS-048, HS-049, HS-051, HS-075, HS-076, HS-077, HS-078, HS-079, HS-080 |
| 11. Notifications | HS-066, HS-067, HS-074 |
| 12. Seeing, merging and shipping results | HS-052, HS-053, HS-054, HS-055, HS-056, HS-057, HS-058 |
| 13. Runaway controls | HS-021, HS-022, HS-023, HS-034 |
| 14. Disk | HS-062, HS-063 |
| 15. Event growth, history and tool output | HS-033, HS-059, HS-060, HS-061, HS-064 |
| 16. Abort ladder, hub gaps | HS-016, HS-022, HS-068, HS-069 |
| Data model and API changes | HS-007, HS-008, HS-016, HS-018, HS-019, HS-033, HS-041, HS-051, HS-052, HS-053, HS-054, HS-057, HS-061, HS-064, HS-066 |
| Single binary, roles, CLI | HS-001, HS-002, HS-003, HS-039, HS-045, HS-050, HS-051, HS-072, HS-073, HS-078, HS-079, HS-080 |
| Config | HS-002 |
| Build and release | HS-001, HS-048, HS-076, HS-080 |
| V1–V11 | HS-027 (V1), HS-045 (V2), HS-040 (V3, V4, V5), HS-038 (V6), HS-041 (V7), HS-035 (V8), HS-044 (V9), HS-001 + HS-003 + HS-004 (V10), HS-070 (V11); HS-046 runs the set |
| Known and accepted risks | HS-027, HS-031, HS-037, HS-039, HS-055, HS-057, HS-063, HS-070 |

## Decisions taken while slicing

Five decisions were made that the spec does not make. Each has an ADR because each is hard to reverse, surprising to a later reader, and the result of a real trade-off.

- [ADR-0001](../adr/0001-build-session-lifecycle-before-docker.md) — the session lifecycle is built against the stub backend before Docker exists. Consequence: there are two fakes, the in-process stub backend and the in-container fake pi, and they are not the same thing.
- [ADR-0002](../adr/0002-security-guards-before-endpoints.md) — the browser guards and network isolation land before the features they protect.
- [ADR-0003](../adr/0003-verification-checks-as-story-gates.md) — V1–V11 are acceptance gates on stories, plus one closing verification run.
- [ADR-0004](../adr/0004-cargo-workspace-with-one-binary-crate.md) — a Cargo workspace with one binary crate. Consequence: `cargo run` from the root needs `-p holosmith`, and `web/` is not a workspace member.
- [ADR-0005](../adr/0005-static-musl-artifacts-proven-without-docker.md) — non-PIE static musl artifacts, proven with `bwrap` and qemu instead of Docker. Consequence: nothing may export `RUSTFLAGS`, and the empty-container run stays open until HS-046.

Gaps in the spec that had to be filled to write these stories, all reversible and all flagged in the story that fills them:

| Gap | Filled as | Story |
| --- | --- | --- |
| No config key for the session cap ("the cap" is never named) | `max_live_sessions`, default 3 | HS-002, HS-020 |
| No config key for the history window | 50 messages, configurable | HS-061 |
| Container resource limits are a risk-table row with no numbers | CPU, memory and PID limits from config | HS-028 |
| `worktree` is `NOT NULL` but empty until provisioning | store `''` for unprovisioned sessions | HS-007 |
| pi project trust is open question 9 | must be resolved and documented inside HS-031 | HS-031 |
| Frontend stack is never named | Vite + TypeScript, single page, no SSR | HS-003 |
| Implementation language is never named | Rust (the spec's `build.rs`/proc-macro constraint only fits Rust) | HS-001 |

## Importing into Linear

**Done.** All 80 stories are live in the **Holosmith** project (`P-PER-2`, team `Personal`/`PER`) as `PER-33` – `PER-112`, in `HS-` order:

https://linear.app/adamolsson/project/holosmith-761104bbb042

| Linear | Stories |
| --- | --- |
| `PER-33` – `PER-38` | HS-001 – HS-006 (Phase 0) |
| `PER-39` – `PER-55` | HS-007 – HS-023 (Phase 1) |
| `PER-56` – `PER-78` | HS-024 – HS-046 (Phase 1b) |
| `PER-79` – `PER-99` | HS-047 – HS-067 (Phase 2) |
| `PER-100` – `PER-106` | HS-068 – HS-074 (Phase 3) |
| `PER-107` – `PER-112` | HS-075 – HS-080 (Phase 4) |

What was set up, and verified against the docs:

- 80 issues, each titled `HS-0NN — <title>`, in **Backlog**, with the user story, demo and acceptance checkboxes in the description.
- 6 **milestones** = the phases (6 / 17 / 23 / 21 / 7 / 6 stories), so each phase has its own progress bar.
- 4 **labels** `size-XS|S|M|L` (workspace-level, since they are useful beyond this project).
- **88 blocking relations**, verified issue-by-issue to match each story's `Blocked by` line exactly; the graph still has exactly one root, `PER-33` (HS-001).

To re-sync after editing the markdown, run `python3 scripts/import-linear.py` — it skips any HS-id already present, so it seeds and extends but never duplicates. `--dry-run` prints the payloads without writing. It reads its credentials from pi's MCP OAuth store and fails with a clear message if `linear` is not authenticated or the token has expired.

Editing an existing story in Linear is not synced back to the markdown. The documents are the source of truth for the plan; treat Linear as the execution surface.

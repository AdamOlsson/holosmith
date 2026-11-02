# One JSON error envelope, and its codes are the spec's names

The spec names the codes and never the body: 403 `bad_host`, `bad_origin`, `bad_client`, `bad_owner` (item 4, HS-005), 409 `session_stopped` (item 8), `unsaved_work` (item 9), `session_running`, `dirty_worktree`, `not_fast_forward`, `conflict`, `target_dirty` (item 12), 503 `draining` (item 10), and a 507 for the free-space floor (item 14). HS-003 is the first story that has to answer with an error at all — an unknown path under `/api/` — so it is the story that fixes the shape, rather than leaving it to whichever endpoint needs an error first.

Every error response that carries a body is `application/json` and `{"code": "<machine code>", "message": "<one human sentence>"}`. The code is a stable snake_case string from one module of constants — never prose, never the transport status; the message is for a human reading a `curl`, and later stories may rewrite it without breaking a client. Status and code together carry the meaning: `404` + `not_found` today, `403` + `bad_host` when HS-005 lands.

Why: later acceptance criteria already speak in codes, so the UI (HS-013 onward) can switch on `code` and a test can assert `code` without pinning a sentence, and HS-005's exemption and error work needs one place that knows what an error looks like. Per-endpoint shapes, the alternative, produce one parser per feature and a UI that guesses.

Consequences: a new code is a constant plus the story that returns it, and that story's test asserts it; there are no HTML error pages, including for a missing asset on a path outside `/api/`, which the router answers with the app shell instead; and `code` values are effectively an API, so renaming one needs a story that says so.

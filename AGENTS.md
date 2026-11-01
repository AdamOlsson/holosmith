# Holosmith

The authoritative specification for this repository is `HolosmithSpec.md`.

## Linear scope

This repository tracks work in exactly one Linear project:

| Field | Value |
|---|---|
| Project | **Holosmith** |
| Project ID | `P-PER-2` |
| Project UUID | `f3a9d020-848a-437b-baec-eba402b25b15` |
| Team | `Personal` (`PER`) |
| URL | https://linear.app/adamolsson/project/holosmith-761104bbb042 |

Rules:

- Always pass `project: "Holosmith"` when calling `list_issues` or `save_issue`.
- When creating an issue, set the Holosmith project and the `PER` team explicitly. Never leave either implicit.
- Never create, read, update, comment on, or close issues in another project. In particular, `VulkanRenderAPI` belongs to a different repository and is out of scope here — do not list it, do not borrow its issue numbers, and do not treat it as precedent.
- If a Linear call cannot be scoped to Holosmith, report that to the user and stop. Do not fall back to a workspace-wide query.

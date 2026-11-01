# Verification checks are gates on stories, not a phase

The spec lists eleven verification checks (V1–V11) with a stated consequence for failure. Rather than a separate verification phase, each check is an acceptance criterion on the story it belongs to, plus one closing story at the end of phase 1 that runs all of them on a real Docker host and records the result.

Why: a check that lives only in a table at the back of a spec is run once, late, when the design it is meant to falsify is already built on. Attached to a story, a failing check blocks that story and the design changes while it is still cheap. The closing story still exists because several checks (V3, V5, V8, V9) only become meaningful once the whole loop runs end to end, and because the spec requires the failures to change the design before phase 2 builds on it.

Cost we accept: some stories carry an acceptance criterion that is a design experiment rather than a feature, so a story can legitimately end in "check failed, design amended" instead of "feature works".

# A project arrives by a command that derives its name

The quick start's third command was `git clone https://github.com/you/project
~/.local/share/holosmith/projects/project`, and the sentence after the block asked the reader to type **the
directory name** into the new-session form. Three things were wrong with it, and none of them was the clone.

- **The path was the reader's to know.** `projects_root` is a config key with a default, so the path in that line is
  only correct for the default and only for a project called `project`. A reader who moved the root, or who cloned
  their own repository, typed a path the command does not take and had nothing to run instead.
- **The name was the reader's to invent.** The form asks for one path segment — letters, digits, `.`, `_` and `-`,
  1 to 255 of them, not `.` or `..` — and a repository called `my project` or a clone left as `project.git` gave a
  name the create answers `422 bad_project` for. From the page that refusal names a rule with no way to see which
  word broke it.
- **Nothing checked what the clone was.** A create refuses a project whose `.git` is not a directory, that has
  submodules, or that enables `extensions.worktreeConfig` (glossary: **Refused project**). `git clone` produced
  all of those happily, so the first sign that a project could not run was a refusal in the browser, minutes
  later, with the repository already on disk.

## D1 — the command, not a path

`holosmith project add <git-url>` clones into `<projects_root>/<name>`, where both halves are read from the config
the rest of the CLI reads; nothing on the command line names a directory. `holosmith project list` prints the
projects that are already there, so the state the old instruction left implicit — what may I type in this form —
is a command too.

The command runs `git clone` as one host child process, the shape `deploy` and `key set` already use for a small
job: no new dependency, no repository library, and the credentials and configuration git already has on the host
are the ones the clone uses. It works with **no server running**: the create path's rules are read from the config
and the filesystem, not over the API, which is why `project add` is usable on a machine whose `serve` is not up
yet.

## D2 — the name is derived from the URL, and validated with the create's own rule

The name is the URL's last path segment, less a trailing `/` and a trailing `.git`, so `…/you/project.git`,
`…/you/project` and `…/you/project/` are one project called `project`. That is the address's own word for itself
rather than a second name the operator has to choose and then remember — and it is the same string a later
`project add` of the same address would derive, so adding the same repository twice is one destination and gets
one refusal.

**The rule is the create's, in one place.** `projects::name_is_valid` is the predicate, and
`http::sessions::validate_project` now calls it and answers `422 bad_project` for exactly what it refuses. A second
copy in the command would have been a second rule to keep in step, and the drift would show up as a project the
command accepted and the form would not take.

**A URL that yields nothing usable is refused with a sentence, never with an invented name.** `https://host/you/..`
and `https://host/` have no segment a create would accept, and `projects::name_from_url` says which segment it
found and what a name may be. A made-up name would not be the directory the reader cloned.

## D3 — what was cloned is put through the create's refusals, and a refused clone is removed

`worktree::refuse` is the one reading of "may this repository be mounted", asked of the clone before the command
answers 0. A refusal prints **the API's own code and sentence** — `git_not_directory`, `submodules_present`,
`worktree_config_enabled` — because the CLI fails with `http::sessions::refusal_error`, the mapping the route
answers with; nothing here re-spells those sentences.

**The fourth answer `refuse` gives is not a refusal, and neither command treats it as one.** `Refusal::Unreadable`
means git could not be asked — the `git config` reading could not be spawned, or it answered with an exit status
that is neither 0 nor 1 — so nothing about the repository has been checked. `add` refuses the clone and prints
git's own words on stderr, which is the one thing that says what went wrong at the terminal the clone ran in, while
the route logs those words and answers ADR-0008's sentence: a client on a phone can do nothing with them. `list`
prints the name — it is there, and the form can be used with it — says on stderr that the project could not be
checked, and **exits 1**, which is `status`'s rule for a fact it could not read. The three coded shapes stay a
silent skip, because a repository the create will not mount is a name the form must not be offered.

**The refused clone is removed before the command exits 1, and the line says so.** A clone left in place would
refuse its own retry: `add` reads an existing destination as taken before it clones anything, so the operator's
next attempt — with the submodules removed, or the worktree config turned off — would answer `already exists`
about a directory they had not made. Removal is safe for a reason and not by luck: the command refuses a
destination that exists *before* it clones, so anything under that path afterwards was written by this run, and a
clone that failed leaves only its own half-written directory.

## D4 — stdout is the name, stderr is everything else

`name=$(holosmith project add https://github.com/you/project.git)` is the use the printed name exists for, so the
name is the only byte on stdout (ADR-0092). `cloning …` is a progress line on stderr, printed before the clone
starts, and every failure is one line on stderr plus exit 1. git's own stream is captured rather than inherited for
that reason: its banner and progress meter would otherwise be lines on the stream a failure has one line of, and
part of what git prints there is nothing a verdict can share a stream with. A failed clone keeps git's **last
non-empty line** in the sentence, which is where a program puts the reason it stopped.

`project list` follows the same split: names on stdout, one per line, nothing else; an absent projects root is an
empty list and exit 0, because the first `add` is what makes the directory. What it prints is filtered by the same
`worktree::refuse` a create applies, so a directory a session cannot be created in is not a name printed for the
form to reject — with the one exception D3 names, where the repository could not be checked at all and the exit
code carries that fact instead of a missing name.

## What measures it

- `crates/holosmith/tests/project_cli.rs` drives the binary the way the README does: a real `git clone` of a local
  repository, the derived name of a URL with `.git` and with a trailing `/`, the occupied destination, a URL that
  names nothing, a `list` before and after an add, the `submodules_present` clone **refused, removed and retried**
  (the retry must reach the refusal again and not `already exists`), a failed clone's one line, and a projects root
  holding one repository git cannot be asked about — its name printed, the stderr line, and exit 1.
- `commands::project`'s unit test is where `git_not_directory` and `worktree_config_enabled` are measured: a fresh
  clone always has a `.git` **directory**, and git writes a clone its own config rather than copying the source's
  local one, so neither shape is reachable through `git clone`. The test asserts each shape's code and sentence
  against the route's constructors, and that the route maps the same `Refusal` to the same pair.
- `projects`' unit tests pin the derivation's table and the name rule at its edges, and `http/sessions.rs`'s
  `a_project_is_one_path_segment` still pins the rule the route answers `bad_project` for.

## Refused

- **A `--name` flag.** It would make the name a second thing to get right, and its wrong value is exactly what the
  form's refusal is about; the derived name is the one a re-add of the same address agrees with.
- **Cloning into a destination that already exists.** Even with the intent of updating it: whether the repository
  there is the one the URL names is a question git answers by refusing, and a replaced checkout would be a project
  whose sessions' worktrees point at a history that changed underneath them.
- **Asking the running server to add the project.** It would need an endpoint, an envelope and a health check
  before the first project exists — and the config the command line already reads holds both the root and, through
  `worktree::refuse`, the rules.
- **A second sentence per refusal, written for the terminal.** The API's sentences are the ones the page shows and
  the ones an operator learns; a shorter terminal spelling would be a second copy that drifts.

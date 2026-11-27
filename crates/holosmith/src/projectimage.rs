//! Which image a project's sessions run, and what a build of it is handed.
//!
//! A project may carry its own recipe at `<project root>/docker/Dockerfile`. When it does, its sessions run
//! `hs-<project>:<digest>` — a content address over the recipe's bytes and the base image's tag, so the whole
//! currency question is "does that tag exist?" and no label is read — and a build of it is handed the project
//! root. When it does not, its sessions run the base image, which has pi, git and certificates and no compiler.
//!
//! **A project with no recipe is supported, not refused.** "Create the project, then write its Dockerfile" is the
//! path this milestone exists to make easy: a session in the base image can commit, ask questions and edit files,
//! and it is `doctor`'s job to say that it cannot compile rather than a create's job to refuse it.
//!
//! [`resolve`] is the one function that answers this, so a session's container, `doctor`, the build and the
//! `image build` verb that follows cannot disagree about the tag or about the directory Docker is handed. Nothing
//! else in the tree may compose `hs-<project>:<digest>`.
//!
//! **`docker/Dockerfile` is the spelling** because the recipe is a project's own file in a project's own
//! repository: it is reviewable in the same diff as the toolchain it installs, and `<project root>` is already
//! where the worktree's repository lives.
//!
//! **The build context is the project root**, filtered by the project's own `.dockerignore` if it has one — which
//! is Docker's read, not this module's: the resolver answers *which directory* and never parses the filter, and an
//! absent `.dockerignore` is not an error. A recipe has to be able to `COPY Cargo.lock web/package-lock.json …`,
//! which is the whole reason a project has a recipe; `docker/` alone as the context would make every project
//! duplicate the lock files it vendors against, giving the vendoring a second source of truth at the moment this
//! work removes the first. `.dockerignore` is a convention an operator already knows.
//!
//! **There are no `org.holosmith.*` labels on a project's image, and none are read.** They are a claim about an
//! artifact whose recipe Holosmith has never read: Holosmith stamps them on the proxy and the base image and on
//! nothing else. A content address needs no claim — two different recipes, or one recipe over a moved base, are
//! two tags ([`tag`]).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The recipe's path inside a project, from the project root: `docker/Dockerfile`.
pub const RECIPE: &str = "docker/Dockerfile";

/// The repository-name prefix of a project's image: `hs-<project>:<digest>`.
pub const NAME_PREFIX: &str = "hs-";

/// Docker's cap on a repository name (`reference.NameTotalLengthMax`), which `hs-<project>` has to fit in.
const NAME_MAX: usize = 255;

/// One project's session image: the tag a container runs, and — when the project carries a recipe — what a build
/// of that tag is handed.
///
/// One value rather than three answers, so the container step, `doctor` and a build cannot disagree: a caller that
/// has resolved a project has the tag and the context together, and there is no second way to ask for either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectImage {
    /// `hs-<project>:<digest>` when the project has a recipe, and the base image's tag when it has none.
    pub image: String,
    /// What a build of [`image`](Self::image) is handed, or `None` when the project has nothing to build.
    pub build: Option<Build>,
}

/// A project's recipe, the directory Docker is handed with it, and the build arguments it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// `<project root>/docker/Dockerfile`: the file the tag's digest is taken over.
    pub recipe: PathBuf,
    /// The project root. Docker filters it by the project's `.dockerignore` when it has one, and an absent one
    /// leaves the context exactly this directory.
    pub context: PathBuf,
    /// The `--build-arg` pairs this recipe **declares**, and only those.
    ///
    /// A `FROM` cannot read a file, so a value a recipe's `FROM` line needs has to arrive as an argument: the base
    /// image's tag and, for a recipe with a rust stage, the pinned rust base's reference. Both are values this binary
    /// already holds — the first is the base a project was resolved against ([`resolve`]'s argument, which is what the
    /// tag's digest covers too), the second is `pins/rust.txt`'s — so filling them here is what keeps a recipe from
    /// carrying a second copy of either (ADR-0118, ADR-0126).
    ///
    /// **The recipe's own text decides which ones.** A `--build-arg` a recipe does not declare is a BuildKit warning
    /// over nothing, and a recipe that declares `HOLOSMITH_RUST_IMAGE` without a rust stage would be a project taking a
    /// value it never uses — so a build is handed exactly the arguments the file names.
    pub arguments: Vec<(String, String)>,
}

/// The argument a project's recipe reads its base image from: the tag the project was resolved against.
pub const BASE_IMAGE_ARG: &str = "HOLOSMITH_BASE_IMAGE";

/// The arguments a project's recipe declares, in the order [`Build::arguments`] hands them over.
///
/// Read from the recipe's own text rather than from a list here, because the recipe is the project's file and this
/// module's job is to fill what it asks for: a `docker/Dockerfile` with no `ARG` at all gets no arguments (an image
/// built `FROM` a literal), one that names the base gets the base's tag, and one that names the rust base gets the
/// pin (ADR-0126).
fn declared_arguments(recipe: &[u8], base_image: &str) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(recipe);
    let mut arguments = Vec::new();
    for line in text.lines() {
        let Some(declared) = line
            .trim()
            .strip_prefix("ARG ")
            .and_then(|rest| rest.split_whitespace().next())
        else {
            continue;
        };
        // `ARG NAME=value` declares a default a build may override; the name is the first word either way.
        let name = declared.split_once('=').map_or(declared, |(name, _)| name);
        let value = match name {
            BASE_IMAGE_ARG => base_image.to_string(),
            crate::images::RUST_IMAGE_ARG => crate::pins::rust_image().to_string(),
            _ => continue,
        };
        if !arguments.iter().any(|(have, _)| have == name) {
            arguments.push((name.to_string(), value));
        }
    }
    arguments
}

/// The image `project`'s sessions run, read from the tree at `project_root` for the base tag `base_image`.
///
/// `base_image` is an argument rather than a read of the config, so this stays a question about one tree and one
/// tag — a unit test needs a temporary directory and nothing else, and no caller can resolve a project against a
/// base its own config does not name.
///
/// The refusals are [`Unusable`]'s: a recipe that is not a regular file, is empty, cannot be read, or a project
/// name Docker cannot spell as a repository. Everything else is an image.
pub fn resolve(
    project_root: &Path,
    project: &str,
    base_image: &str,
) -> Result<ProjectImage, Unusable> {
    let recipe = project_root.join(RECIPE);
    // `symlink_metadata`, so a link at the recipe's path is something rather than nothing: a link to nothing is a
    // recipe that cannot be built, and reporting it as absent would run a session in the base image while the
    // operator's file sits there looking like it was used.
    match std::fs::symlink_metadata(&recipe) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(ProjectImage {
                image: base_image.to_string(),
                build: None,
            });
        }
        // A path under a directory this process cannot read is not "no recipe": nothing was measured.
        Err(error) => {
            return Err(Unusable::Unreadable {
                path: recipe,
                error: error.to_string(),
            });
        }
        Ok(_) => {}
    }
    // Followed, so a link to a file is that file.
    if !std::fs::metadata(&recipe).is_ok_and(|metadata| metadata.is_file()) {
        return Err(Unusable::NotAFile { path: recipe });
    }
    // Before the read, because a name Docker cannot spell is a project no recipe can rescue — and never a silent
    // lowercase: `MyRepo` and `myrepo` are two projects, and one tag for both is one project's image run by the
    // other's sessions.
    if !repository_name_is_valid(&format!("{NAME_PREFIX}{project}")) {
        return Err(Unusable::NameNotDocker {
            project: project.to_string(),
            path: recipe,
        });
    }
    let bytes = std::fs::read(&recipe).map_err(|error| Unusable::Unreadable {
        path: recipe.clone(),
        error: error.to_string(),
    })?;
    // Zero bytes or whitespace only: a file Docker would refuse with a sentence about a missing `FROM`, arriving
    // where the operator can do nothing with it.
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(Unusable::Empty { path: recipe });
    }
    Ok(ProjectImage {
        image: tag(project, &bytes, base_image),
        build: Some(Build {
            recipe,
            context: project_root.to_path_buf(),
            arguments: declared_arguments(&bytes, base_image),
        }),
    })
}

/// `hs-<project>:<digest>`: the tag a project's own recipe is built and run under.
///
/// The digest covers the recipe's bytes **and** the base image's tag, and nothing else. A full SHA-256 in hex is
/// 64 characters, inside Docker's 128-character tag limit, so nothing is truncated: a shortened digest would be a
/// second thing an operator has to be told.
///
/// **What it does not cover is the files the recipe copies.** A `COPY Cargo.lock …` over a lock that has moved is
/// invisible to both inputs: the tag is the same and the image it names is stale. That is the one staleness a
/// content address over the recipe cannot catch, and it is deliberate — a digest over the tree would move on every
/// `cargo build`, which is not a content address of anything an operator wrote. The change that closes it owns it.
fn tag(project: &str, recipe: &[u8], base_image: &str) -> String {
    let mut input = Vec::with_capacity(8 + recipe.len() + base_image.len());
    // The recipe's length first, as a fixed-width prefix. The recipe is arbitrary bytes and the base tag is the
    // tail, so a bare concatenation would let a recipe ending in the base tag's characters hash the same as a
    // different recipe over a different base: `b"a" + "bc"` and `b"ab" + "c"` are one string. Length-prefixing the
    // variable half is what makes the split unambiguous whatever the two hold.
    input.extend_from_slice(&(recipe.len() as u64).to_be_bytes());
    input.extend_from_slice(recipe);
    input.extend_from_slice(base_image.as_bytes());
    format!(
        "{NAME_PREFIX}{project}:{}",
        crate::images::hex(&crate::images::sha256(&input))
    )
}

/// Whether `name` is a Docker repository name: lowercase alphanumerics in components separated by `.`, `_`, `__`
/// or a run of `-`, and no longer than [`NAME_MAX`].
///
/// Docker's own grammar (`distribution/reference`'s `pathComponent`), written out rather than approximated,
/// because a project name is a wider alphabet than a repository name: `projects::name_is_valid` accepts `MyRepo`,
/// `Repo.git`, `-repo` and `.repo`, and each of those is a name Docker refuses. A recipe under one of them is a
/// refusal naming the path rather than a tag Docker would refuse three steps later.
fn repository_name_is_valid(name: &str) -> bool {
    if name.is_empty() || name.len() > NAME_MAX {
        return false;
    }
    let bytes = name.as_bytes();
    let mut index = 0;
    // True when the byte before `index` completed a component, which is the only place a separator may start.
    let mut after_component = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            after_component = true;
            index += 1;
        } else if after_component {
            match byte {
                b'-' => {
                    while bytes.get(index) == Some(&b'-') {
                        index += 1;
                    }
                }
                b'_' => {
                    index += 1;
                    // `__` is one separator in Docker's grammar; `_._` and `__-` are not separators at all.
                    if bytes.get(index) == Some(&b'_') {
                        index += 1;
                    }
                }
                b'.' => index += 1,
                _ => return false,
            }
            after_component = false;
        } else {
            return false;
        }
    }
    // A trailing separator leaves the last component empty, which is not a component.
    after_component
}

/// Why a project's recipe cannot become the image its sessions run.
///
/// Each arm names the path, because the operator's next step is an edit in that file: the create answers
/// [`sentence`](Self::sentence) with `422 unusable_image`, and `doctor` prints it as information rather than as a
/// failure — a machine whose project needs an edit is a machine `serve` starts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    /// The path is there and is not a regular file: a directory, a socket, a symlink to nothing.
    NotAFile { path: PathBuf },
    /// It holds zero bytes, or nothing but whitespace.
    Empty { path: PathBuf },
    /// It could not be read, and a recipe nobody read is not a recipe that is absent.
    Unreadable { path: PathBuf, error: String },
    /// The project's name is not one Docker can spell as a repository, so its image has no name to build under.
    NameNotDocker { project: String, path: PathBuf },
}

impl Unusable {
    /// The one sentence the operator reads: the path, what is wrong with it, and what to do.
    pub fn sentence(&self) -> String {
        match self {
            Self::NotAFile { path } => format!(
                "{} is not a regular file: a project's image recipe has to be one, so this project's sessions \
                 cannot start until it is a Dockerfile or is removed",
                path.display()
            ),
            Self::Empty { path } => format!(
                "{} is empty: a project's image recipe has to say what it builds on, so this project's sessions \
                 cannot start until it is written or is removed",
                path.display()
            ),
            Self::Unreadable { path, error } => format!(
                "{} could not be read ({error}): a project's sessions run the image its recipe builds, so nothing \
                 can say which image that is until the file is readable",
                path.display()
            ),
            Self::NameNotDocker { project, path } => format!(
                "the project's name `{project}` is not one Docker can spell as a repository, so {} cannot name an \
                 image: a project with a recipe needs a name of lowercase letters, digits, `.`, `_` and `-` — \
                 rename the directory, or remove the recipe so its sessions run the base image",
                path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory named for the test, made fresh and removed by the test that made it.
    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "holosmith-projectimage-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the scratch directory is creatable");
        path
    }

    /// A project with `docker/Dockerfile` holding `recipe`.
    fn project(root: &Path, name: &str, recipe: &[u8]) -> PathBuf {
        let path = root.join(name);
        std::fs::create_dir_all(path.join("docker")).expect("the recipe's directory is creatable");
        std::fs::write(path.join(RECIPE), recipe).expect("the recipe is writable");
        path
    }

    #[test]
    fn two_recipes_are_two_tags_and_one_recipe_twice_is_one() {
        let root = scratch("two-recipes");
        let base = "holosmith-base:0.1.0";
        let one = project(
            &root,
            "one",
            b"FROM holosmith-base:0.1.0\nRUN apt-get install -y make\n",
        );
        let first = resolve(&one, "one", base).expect("a written recipe resolves");
        let again = resolve(&one, "one", base).expect("a written recipe resolves");
        assert_eq!(first.image, again.image, "one recipe gave two tags");
        // The same project, one line of its recipe changed: the digest is the recipe's, so the tag moves.
        std::fs::write(
            one.join(RECIPE),
            b"FROM holosmith-base:0.1.0\nRUN apt-get install -y jq\n",
        )
        .expect("the recipe is writable");
        let second = resolve(&one, "one", base).expect("a written recipe resolves");
        assert_ne!(
            first.image, second.image,
            "two recipes over one base gave one tag"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_same_recipe_over_a_moved_base_is_a_new_tag() {
        let root = scratch("moved-base");
        let one = project(&root, "one", b"FROM holosmith-base:0.1.0\n");
        let old = resolve(&one, "one", "holosmith-base:0.1.0").expect("a written recipe resolves");
        let new = resolve(&one, "one", "holosmith-base:0.2.0").expect("a written recipe resolves");
        assert_ne!(old.image, new.image, "a moved base kept the tag");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_project_with_no_recipe_runs_the_base_image_and_has_nothing_to_build() {
        let root = scratch("no-recipe");
        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).expect("the project is creatable");
        let resolved =
            resolve(&plain, "plain", "holosmith-base:0.1.0").expect("no recipe is not a refusal");
        assert_eq!(resolved.image, "holosmith-base:0.1.0");
        assert_eq!(resolved.build, None, "a project with no recipe had a build");
        // A `docker/` directory with no Dockerfile in it is the same answer: the file is what makes a recipe.
        std::fs::create_dir_all(plain.join("docker")).expect("the directory is creatable");
        assert_eq!(
            resolve(&plain, "plain", "holosmith-base:0.1.0")
                .expect("no recipe is not a refusal")
                .build,
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_or_blank_recipe_is_refused_naming_the_path() {
        let root = scratch("empty-recipe");
        for (name, bytes) in [("zero", b"".as_slice()), ("blank", b" \n\t\n".as_slice())] {
            let path = project(&root, name, bytes);
            let problem = resolve(&path, name, "holosmith-base:0.1.0").expect_err(name);
            assert_eq!(
                problem,
                Unusable::Empty {
                    path: path.join(RECIPE)
                },
                "{name}"
            );
            let sentence = problem.sentence();
            assert!(
                sentence.contains(&path.join(RECIPE).display().to_string()),
                "the sentence does not name the recipe: {sentence}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_recipe_path_that_is_not_a_regular_file_is_refused_naming_the_path() {
        let root = scratch("not-a-file");
        // A directory where the recipe should be.
        let directory = root.join("directory");
        std::fs::create_dir_all(directory.join(RECIPE)).expect("the directory is creatable");
        let problem = resolve(&directory, "directory", "holosmith-base:0.1.0")
            .expect_err("a directory is not a recipe");
        assert_eq!(
            problem,
            Unusable::NotAFile {
                path: directory.join(RECIPE)
            }
        );
        assert!(
            problem
                .sentence()
                .contains(&directory.join(RECIPE).display().to_string())
        );
        // A link to nothing, which `symlink_metadata` is what makes visible: followed, it would read as absent and
        // the operator's file would sit there looking used.
        let dangling = root.join("dangling");
        std::fs::create_dir_all(dangling.join("docker")).expect("the directory is creatable");
        std::os::unix::fs::symlink(dangling.join("gone"), dangling.join(RECIPE))
            .expect("the link is creatable");
        let problem = resolve(&dangling, "dangling", "holosmith-base:0.1.0")
            .expect_err("a link to nothing is not a recipe");
        assert_eq!(
            problem,
            Unusable::NotAFile {
                path: dangling.join(RECIPE)
            }
        );
        // And a link to a file really is that file, which is the arm the one above must not swallow.
        let linked = root.join("linked");
        std::fs::create_dir_all(linked.join("docker")).expect("the directory is creatable");
        let real = root.join("elsewhere.Dockerfile");
        std::fs::write(&real, b"FROM holosmith-base:0.1.0\n").expect("the recipe is writable");
        std::os::unix::fs::symlink(&real, linked.join(RECIPE)).expect("the link is creatable");
        let resolved = resolve(&linked, "linked", "holosmith-base:0.1.0")
            .expect("a link to a file is a recipe");
        assert!(
            resolved.image.starts_with("hs-linked:"),
            "{}",
            resolved.image
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_context_is_the_project_root_with_and_without_a_dockerignore() {
        let root = scratch("context");
        let one = project(&root, "one", b"FROM holosmith-base:0.1.0\n");
        let without = resolve(&one, "one", "holosmith-base:0.1.0")
            .expect("a written recipe resolves")
            .build
            .expect("a recipe has a build");
        assert_eq!(without.context, one, "the context is not the project root");
        // A filter is Docker's read of a file in the context, never this function's: the directory it answers is
        // the same one either way, and an absent `.dockerignore` is not an error.
        std::fs::write(one.join(".dockerignore"), b"target/\nnode_modules/\n")
            .expect("the filter is writable");
        let with = resolve(&one, "one", "holosmith-base:0.1.0")
            .expect("a written recipe resolves")
            .build
            .expect("a recipe has a build");
        assert_eq!(with.context, one, "a `.dockerignore` moved the context");
        assert_eq!(without.recipe, with.recipe);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_recipe_that_declares_no_argument_is_handed_none() {
        // The rule is the recipe's own text: a project whose `FROM` is a literal gets no `--build-arg` at all, because an
        // argument a recipe does not declare is a BuildKit warning over nothing (ADR-0126).
        let root = scratch("no-arguments");
        let one = project(
            &root,
            "one",
            b"FROM holosmith-base:0.1.0\nRUN apt-get install -y make\n",
        );
        let resolved =
            resolve(&one, "one", "holosmith-base:0.1.0").expect("a written recipe resolves");
        let build = resolved.build.expect("a recipe has a build");
        assert_eq!(build.arguments, Vec::<(String, String)>::new());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_recipe_that_declares_the_two_arguments_is_handed_exactly_those() {
        // Both values are this binary's, and neither may be a recipe's own copy: the base tag is the one the project was
        // resolved against — so the tag's digest and the `FROM` agree — and the rust reference is `pins/rust.txt`'s
        // (ADR-0118). A recipe declaring one of them gets that one and nothing else.
        let root = scratch("arguments");
        let recipe = b"ARG HOLOSMITH_BASE_IMAGE=holosmith-base:latest\nARG HOLOSMITH_RUST_IMAGE\nFROM rust:${HOLOSMITH_RUST_IMAGE} AS rust\nFROM ${HOLOSMITH_BASE_IMAGE}\n";
        let both = project(&root, "both", recipe);
        let resolved =
            resolve(&both, "both", "holosmith-base:0.7.0").expect("a written recipe resolves");
        assert_eq!(
            resolved.build.expect("a recipe has a build").arguments,
            [
                (
                    BASE_IMAGE_ARG.to_string(),
                    "holosmith-base:0.7.0".to_string()
                ),
                (
                    crate::images::RUST_IMAGE_ARG.to_string(),
                    crate::pins::rust_image().to_string()
                ),
            ],
            "the arguments a build is handed are not the recipe's own, filled with this binary's values"
        );
        // One of them, and only it: a recipe with no rust stage is not handed the rust reference.
        let base_only = project(
            &root,
            "base-only",
            b"ARG HOLOSMITH_BASE_IMAGE\nFROM ${HOLOSMITH_BASE_IMAGE}\n",
        );
        let resolved = resolve(&base_only, "base-only", "holosmith-base:0.7.0")
            .expect("a written recipe resolves");
        assert_eq!(
            resolved.build.expect("a recipe has a build").arguments,
            [(
                BASE_IMAGE_ARG.to_string(),
                "holosmith-base:0.7.0".to_string()
            )]
        );
        // And a declared default is not a second source: `ARG NAME=value` is read for its name, and the value a build is
        // handed is the one the resolver holds.
        let defaulted = project(
            &root,
            "defaulted",
            b"ARG HOLOSMITH_BASE_IMAGE=holosmith-base:latest\nFROM ${HOLOSMITH_BASE_IMAGE}\n",
        );
        let resolved = resolve(&defaulted, "defaulted", "holosmith-base:0.7.0")
            .expect("a written recipe resolves");
        assert_eq!(
            resolved.build.expect("a recipe has a build").arguments,
            [(
                BASE_IMAGE_ARG.to_string(),
                "holosmith-base:0.7.0".to_string()
            )],
            "a recipe's declared default displaced the tag the project was resolved against"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tag_is_the_projects_name_and_a_full_hex_digest() {
        let root = scratch("tag-shape");
        let one = project(&root, "my.repo_2-x", b"FROM holosmith-base:0.1.0\n");
        let resolved = resolve(&one, "my.repo_2-x", "holosmith-base:0.1.0")
            .expect("a written recipe resolves");
        let (name, digest) = resolved
            .image
            .split_once(':')
            .unwrap_or_else(|| panic!("the tag is not `<name>:<digest>`: {}", resolved.image));
        assert_eq!(name, "hs-my.repo_2-x");
        assert_eq!(digest.len(), 64, "the digest is not a full SHA-256");
        assert!(
            digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "the digest is not lowercase hex: {digest}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_project_name_docker_cannot_spell_is_refused_naming_the_recipe() {
        let root = scratch("name");
        let base = "holosmith-base:0.1.0";
        for name in [
            "MyRepo", ".repo",
            // `-repo` is not here: the `hs-` prefix puts an alphanumeric before the dashes, and Docker's
            // separator grammar takes a run of them, so a project named `-repo` can be spelled.
            "repo-", "a..b", "a b",
        ] {
            // `.repo` and `a b` are not names the create would take, and are here because the rule is the
            // repository's: this function answers a tree, whatever asked about it.
            let path = project(&root, &format!("p{name}"), b"FROM holosmith-base:0.1.0\n");
            let problem = resolve(&path, name, base).expect_err(name);
            match &problem {
                Unusable::NameNotDocker { project, path } => {
                    assert_eq!(project, name);
                    assert!(path.ends_with(RECIPE));
                }
                other => panic!("{name} answered {other:?}"),
            }
            let sentence = problem.sentence();
            assert!(sentence.contains(name), "{sentence}");
            assert!(sentence.contains(RECIPE), "{sentence}");
        }
        // The longest name Docker takes, and the first it does not: `hs-` is three of the 255 bytes.
        let longest = "a".repeat(252);
        let path = project(&root, &longest, b"FROM holosmith-base:0.1.0\n");
        assert!(
            resolve(&path, &longest, base).is_ok(),
            "252 characters was refused"
        );
        let longer = "a".repeat(253);
        let path = project(&root, &longer, b"FROM holosmith-base:0.1.0\n");
        assert!(
            matches!(
                resolve(&path, &longer, base),
                Err(Unusable::NameNotDocker { .. })
            ),
            "253 characters was accepted"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_repository_name_is_dockers_grammar() {
        for name in [
            "hs-repo",
            "hs-a.b",
            "hs-a_b",
            "hs-a__b",
            "hs-a--b",
            // The `hs-` prefix's own dash and the project's: one run of dashes is one separator.
            "hs--repo",
            "hs-2",
            "hs-a-b.c_d",
        ] {
            assert!(repository_name_is_valid(name), "{name} was refused");
        }
        for name in [
            "", "hs-", "hs--", "hs-.a", "hs-a.", "hs-a..b", "hs-a._b", "hs-A", "hs-a/b", "hs-a b",
        ] {
            assert!(!repository_name_is_valid(name), "{name} was accepted");
        }
    }
}

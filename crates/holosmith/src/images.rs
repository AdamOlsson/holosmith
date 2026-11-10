//! The two images `server deploy`'s step 3 builds, from the Dockerfiles embedded here (HS-048).
//!
//! The Dockerfiles are `include_str!`ed rather than shipped beside the binary: a deploy runs on a machine that
//! has the artifact and nothing else, so the recipe has to be *inside* it, and `scripts/check-static.sh` greps
//! the built artifact for each file's marker to prove it is. The bodies are in `images/`, because a Dockerfile
//! written as a Rust string literal is a file no linter reads and no human diffs.
//!
//! Three things about a build are load-bearing and each has its own doc comment below:
//!
//! - **The daemon is handed a tar on stdin**, not a directory: `docker build -` reads a context from an
//!   archive, which is the only way to build without a directory on disk that holds a copy of this binary.
//! - **The image's labels are the read.** `server deploy`'s second run must be honestly `satisfied`, and the
//!   five `org.holosmith.*` labels are what says an image was built by *this* binary — not a tag, which any
//!   `docker tag` can move, and not a build timestamp, which says nothing about what is inside. One of them is a
//!   digest of the bytes the build copies, because `git describe --dirty` names the same tree twice and the
//!   acceptance's word is *the binary*.
//! - **The architecture is checked once, before the first build.** A daemon on another machine (a
//!   `docker_host` pointing at a socket elsewhere) builds images for *its* architecture, and an image whose
//!   base is `arm64` cannot run the `x86_64` binary the archive put in it.
//!
//! [`Images`] is the seam, for the reason [`crate::proxies::Containers`] is one: four daemon questions are
//! what a build asks, and a test drives the whole read-and-build path through a fake without a daemon.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::config::Config;
use crate::docker::Docker;
use crate::pins;

/// The proxy image's Dockerfile, as the file in `crates/holosmith/images/`.
pub const PROXY_DOCKERFILE: &str = include_str!("../images/proxy.Dockerfile");

/// The agent image's Dockerfile, for the same reason.
pub const AGENT_DOCKERFILE: &str = include_str!("../images/agent.Dockerfile");

/// The **smoke** image's recipe (HS-077, spec item 10 step 2): the candidate as `pi`, on the agent image's base.
///
/// A third recipe rather than a variant of the agent image, because the two answer different questions: the agent
/// image is what a session runs in production (pi at the pinned version, git, the toolchains), and this one exists so
/// a *candidate binary* can prove itself with a `pi` that replays a recording. Built by `holosmith smoke`, never by
/// `server deploy` — it is not what a promotion ships, and the ADR says so.
pub const SMOKE_DOCKERFILE: &str = include_str!("../images/smoke.Dockerfile");

/// The smoke image's tag: `holosmith-smoke:<version>`, one spelling for `images` and `smoke` (HS-077).
pub fn smoke_image() -> String {
    format!("holosmith-smoke:{}", env!("CARGO_PKG_VERSION"))
}

/// Where the binary lives inside both images (`--entrypoint`, and `holosmith container snapshot`).
pub const BINARY_IN_IMAGE: &str = "/usr/local/bin/holosmith";

/// The five labels an image carries (four on the proxy, plus the toolchain list on the agent), all under this
/// build's own namespace.
///
/// Read back by step 3's skip rule and by `doctor`, so the spelling is a contract: an image built by another
/// tool and labelled differently is "not built from this binary's Dockerfile" rather than "close enough".
pub const LABEL_VERSION: &str = "org.holosmith.version";
pub const LABEL_GIT_TAG: &str = "org.holosmith.git-tag";
pub const LABEL_PI: &str = "org.holosmith.pi-version";
pub const LABEL_TOOLCHAINS: &str = "org.holosmith.toolchains";
/// The SHA-256 of the binary bytes the image was built from — the label that makes the skip rule see a
/// **rebuilt** artifact.
///
/// The git tag alone cannot answer that question. `HOLOSMITH_GIT_TAG` is `git describe --tags --always --dirty`
/// (`build.rs`), so two builds of the same commit — or of the same *dirty tree*, which is what a working tree
/// gets — carry the same tag while their bytes differ: edit a file, rebuild, deploy, and a label set of
/// version + tag + pi says `satisfied` over an image holding the previous binary, while `hs_egress` keeps
/// running it. The acceptance's word is "the binary", so the read measures the binary.
pub const LABEL_BINARY: &str = "org.holosmith.binary-sha256";

/// One image build's bound: a first build installs pi over the network.
///
/// Twenty minutes rather than the proxies' minute, because the agent image's `npm install` fetches pi and the
/// toolchains' packages from two package managers on a machine whose images are cold. It is a **bound** and not
/// a timeout to fail by: `Ok(None)` from the build is "the build did not finish", which stops the step with a
/// sentence rather than hanging a deploy nobody is watching.
pub const BUILD_TIMEOUT: Duration = Duration::from_secs(1200);

/// `holosmith-proxy:<version>`: the image the machine's two proxies run, named from this binary.
///
/// The version rather than `latest`: an upgraded binary must build and name a different image, and a tag an
/// operator can read back is the one fact `docker image ls` shows of what a deploy installed.
pub fn proxy_image() -> String {
    format!("holosmith-proxy:{}", env!("CARGO_PKG_VERSION"))
}

/// `agent-image:<version>`: the image a session's container runs, the same name `agent_image` defaults to.
pub fn agent_image() -> String {
    format!("agent-image:{}", env!("CARGO_PKG_VERSION"))
}

/// Which of the two images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Proxy,
    Agent,
}

impl Kind {
    /// The Dockerfile the daemon is handed, out of the archive it reads.
    pub fn dockerfile(self) -> &'static str {
        match self {
            Self::Proxy => PROXY_DOCKERFILE,
            Self::Agent => AGENT_DOCKERFILE,
        }
    }

    /// The name this binary builds and tags it by.
    pub fn image(self) -> String {
        match self {
            Self::Proxy => proxy_image(),
            Self::Agent => agent_image(),
        }
    }

    /// The label keys this image carries: the proxy's three, and the agent's plus its toolchain list.
    ///
    /// A list rather than four constants compared by hand, so that "which labels does this image have" is one
    /// answer that [`Expectation::mismatches`] and the tests both read.
    /// The label keys this image carries: the proxy's four, and the agent's plus its toolchain list.
    ///
    /// A list rather than five constants compared by hand, so that "which labels does this image have" is one
    /// answer that [`Expectation::mismatches`] and the tests both read.
    pub fn tags(self) -> &'static [&'static str] {
        match self {
            Self::Proxy => &[LABEL_VERSION, LABEL_GIT_TAG, LABEL_PI, LABEL_BINARY],
            Self::Agent => &[
                LABEL_VERSION,
                LABEL_GIT_TAG,
                LABEL_PI,
                LABEL_BINARY,
                LABEL_TOOLCHAINS,
            ],
        }
    }
}

/// What one image's labels must say, from this binary's own values.
///
/// Built from `env!` at compile time, from the config's `agent_toolchains`, and from a **digest of the binary
/// bytes the build will copy**: an image is current when its labels are exactly this, which is why nothing here
/// reads a build timestamp or an image id — a rebuild of identical inputs would change both and still be
/// current. The binary is in the list because it is the one input a tag cannot describe: two builds of the same
/// commit (or of the same dirty tree) carry the same `git describe`, and an image built from the older one would
/// otherwise read as current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectation {
    pub version: String,
    pub git_tag: String,
    pub pi_version: String,
    /// Lowercase hex SHA-256 of the program the build's archive hands the daemon.
    pub binary: String,
    pub toolchains: Option<Vec<String>>,
}

impl Expectation {
    /// This binary's values for `kind`, with the toolchains only where the Dockerfile has them.
    ///
    /// It reads `binary`, because the digest is a property of those bytes and nothing else: a `Expectation`
    /// without one would be a skip rule blind to the very thing the acceptance names, so the constructor cannot
    /// produce it.
    pub fn of(kind: Kind, config: &Config, binary: &Path) -> Result<Self, String> {
        Ok(Self::of_digest(kind, config, &digest(binary)?))
    }

    /// The same expectation from a digest the caller already computed.
    ///
    /// This is what `server deploy` and `doctor` use, through [`own_digest`]: the value is *this process's* bytes,
    /// it cannot change while the process runs, and hashing a multi-megabyte artifact once per step 3 is work
    /// nobody asked for. Nothing can invent a digest this way that is not a file's: the only producer of the
    /// argument in the product is [`own_digest`], and the tests take theirs from the same function or from
    /// [`digest`] over the artifact they build.
    pub fn of_digest(kind: Kind, config: &Config, binary: &str) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_tag: env!("HOLOSMITH_GIT_TAG").to_string(),
            pi_version: pins::pi_version().to_string(),
            binary: binary.to_string(),
            toolchains: (kind == Kind::Agent).then(|| config.agent_toolchains.clone()),
        }
    }

    /// The label keys this image is expected to carry: the agent's four, the proxy's three.
    ///
    /// Derived from `toolchains` rather than carried as a second field, because the two cannot disagree that
    /// way: an `Expectation` with a toolchain list is the agent image's.
    fn keys(&self) -> &'static [&'static str] {
        match self.toolchains {
            Some(_) => Kind::Agent.tags(),
            None => Kind::Proxy.tags(),
        }
    }

    /// What one label must hold, or `None` for a key this image does not carry.
    fn value(&self, key: &str) -> Option<String> {
        match key {
            LABEL_VERSION => Some(self.version.clone()),
            LABEL_GIT_TAG => Some(self.git_tag.clone()),
            LABEL_PI => Some(self.pi_version.clone()),
            LABEL_BINARY => Some(self.binary.clone()),
            LABEL_TOOLCHAINS => self
                .toolchains
                .as_ref()
                .map(|toolchains| toolchains.join(" ")),
            _ => None,
        }
    }

    /// One sentence per key that is absent or different; empty means the labels are this binary's.
    ///
    /// The sentence names the label by its last word (`pi-version`, not `org.holosmith.pi-version`) and carries
    /// both values, because the operator's question is "what did I build against" and the fix is a rebuild.
    ///
    /// An **empty toolchain list** is the one expectation a missing label satisfies: `--build-arg
    /// HOLOSMITH_TOOLCHAINS=` gives the Dockerfile an empty value, and whether a `LABEL key=""` survives in
    /// `Config.Labels` is the daemon's business, not the machine's. A machine whose image installs no extra
    /// package must not rebuild on every deploy (nor fail `doctor`) because of an empty string.
    pub fn mismatches(&self, labels: &BTreeMap<String, String>) -> Vec<String> {
        let mut sentences = Vec::new();
        for key in self.keys() {
            let Some(wanted) = self.value(key) else {
                continue;
            };
            match labels.get(*key) {
                Some(found) if *found == wanted => {}
                None if wanted.is_empty() => {}
                Some(found) => sentences.push(format!(
                    "its {} label says {found}, and this binary's is {wanted}",
                    label_word(key)
                )),
                None => sentences.push(format!(
                    "its {} label is absent, and this binary's is {wanted}",
                    label_word(key)
                )),
            }
        }
        sentences
    }

    /// Whether **none** of this image's keys is present, which is "built by something other than this binary's
    /// Dockerfile" (or by a release old enough to have written no labels).
    ///
    /// Docker answers a **nil map** for an image that carries no labels at all — `null` from
    /// `{{json .Config.Labels}}`, which [`parse_labels`] turns into an empty map — so "no labels" and "someone
    /// else's labels and none of ours" arrive here as the same answer, and both mean the same thing: there is
    /// nothing to compare, rather than something that compared unequal. That distinction is the whole reason this
    /// is a predicate instead of an `is_empty()`: an image carrying one of the keys with a different value is
    /// *not* unbuilt, and must be reported key by key.
    pub fn unbuilt(&self, labels: &BTreeMap<String, String>) -> bool {
        self.keys().iter().all(|key| !labels.contains_key(*key))
    }

    /// `(key, value)` pairs as the Dockerfile receives them: `--build-arg KEY=value`, once per pair.
    ///
    /// The toolchain list is one space-separated value, because that is what `${HOLOSMITH_TOOLCHAINS}` has to be
    /// for the `apt-get install` line to read it as several packages — and the image's label carries the same
    /// string, because a `LABEL` line has no way to rewrite what it was given. One spelling on both sides is what
    /// makes the read and the build agree.
    pub fn build_args(&self) -> Vec<(String, String)> {
        let mut pairs = vec![
            ("HOLOSMITH_VERSION".to_string(), self.version.clone()),
            ("HOLOSMITH_GIT_TAG".to_string(), self.git_tag.clone()),
            ("HOLOSMITH_PI_VERSION".to_string(), self.pi_version.clone()),
            ("HOLOSMITH_BINARY_SHA256".to_string(), self.binary.clone()),
        ];
        if let Some(toolchains) = &self.toolchains {
            pairs.push(("HOLOSMITH_TOOLCHAINS".to_string(), toolchains.join(" ")));
        }
        pairs
    }
}

/// The last word of a label key, which is what its sentence calls it.
fn label_word(key: &str) -> &'static str {
    match key {
        LABEL_VERSION => "version",
        LABEL_GIT_TAG => "git-tag",
        LABEL_PI => "pi-version",
        LABEL_BINARY => "binary-sha256",
        LABEL_TOOLCHAINS => "toolchains",
        _ => "label",
    }
}

/// The SHA-256 of a file, lowercase hex: what `org.holosmith.binary-sha256` carries (HS-048).
///
/// Written here rather than taken from a crate: this build's dependencies are fixed (`--locked`), the only
/// question asked of it is "are these the same bytes", and forty lines checked against the standard's own test
/// vectors are easier to trust than a dependency nothing else needs. It is **not** a security boundary — the
/// producer and the consumer of the label are the same program — but it is the real algorithm, so an operator
/// can compare it with `sha256sum` on the artifact and see why a deploy wants to rebuild.
pub fn digest(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "the binary at {} could not be read for its digest ({error}): an image's labels name the bytes it was \
             built from, and a read that cannot measure them cannot say whether an image is current",
            path.display()
        )
    })?;
    Ok(hex(&sha256(&bytes)))
}

/// The SHA-256 of **this process's own binary**, hashed once per process.
///
/// Which binary a deploy builds from and which one an image's label must match are the same question — this
/// one — so it is asked once. A failed read is cached too: a process whose own path cannot be read will not
/// grow one, and a second failure costs the same sentence.
pub fn own_digest() -> Result<String, String> {
    static DIGEST: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let path = std::env::current_exe().map_err(|error| {
                format!(
                    "this process's own path could not be read ({error}): an image's labels name the bytes it \
                     was built from, so nothing can say whether an image is current without them"
                )
            })?;
            digest(&path)
        })
        .clone()
}

/// Lowercase hex, two characters per byte.
///
/// `pub(crate)` since HS-073, so `remote`'s digest of the file it sends is written by the same two functions
/// [`digest`] and [`own_digest`] use rather than by a third hex writer beside them.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// SHA-256 (FIPS 180-4), the compression function written out.
///
/// The test vectors at the bottom of this module are the standard's: `""`, `"abc"` and the two-block
/// `"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"`, which is one block's worth of padding plus a
/// tail and catches an off-by-one in the length field.
///
/// **Shared with `push::crypto` since HS-066**, which builds HMAC-SHA-256 on it rather than writing the standard
/// a second time: a Web Push message's key schedule is three HMACs deep (RFC 8291 §3.4), and two SHA-256s in one
/// crate would be two things to keep right.
pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    // Padding: one set bit, zeros to 56 mod 64, then the message's length in bits as a big-endian u64.
    let mut message = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());
    // `as_chunks` rather than `chunks_exact`: the padding above makes the length a multiple of the block, so the
    // remainder is empty by construction and the compiler can see that.
    for block in message.as_chunks::<64>().0 {
        let mut schedule = [0_u32; 64];
        for (index, word) in block.as_chunks::<4>().0.iter().enumerate() {
            schedule[index] = u32::from_be_bytes(*word);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut out = [0_u8; 32];
    for (chunk, value) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    out
}

/// The child's answer: whether the build succeeded, and its combined output.
///
/// Both halves are needed: a failure's sentence is parsed out of the output, and a success prints nothing worth
/// reading but is still the step's evidence. It is one value rather than `Result<(), String>` because a failed
/// build is an *answer* — the daemon ran, decided, and said why — and only a daemon that could not be asked at
/// all is a `Result`'s `Err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Log {
    pub success: bool,
    pub text: String,
}

/// The daemon questions an image build asks, and nothing else.
///
/// [`Docker`] is the production implementation and the only one; a fake exists for the tests that drive step 3
/// without a daemon (see the module docs).
pub trait Images {
    /// `docker version --format '{{.Server.Arch}}'` → `amd64`/`arm64` (Docker's words).
    fn daemon_arch(&self) -> Result<String, String>;

    /// The label map of `image`: `Ok(Some(map))` when it is there, `Ok(None)` when it is not, and
    /// `Ok(Some(empty))` for an image that carries no labels at all.
    fn labels(&self, image: &str) -> Result<Option<BTreeMap<String, String>>, String>;

    /// `docker build -t <tag> --build-arg … -`, with `archive` on its stdin.
    fn build(&self, tag: &str, archive: &[u8], args: &[String]) -> Result<Log, String>;
}

impl Images for Docker {
    fn daemon_arch(&self) -> Result<String, String> {
        Docker::run(self, &["version", "--format", "{{.Server.Arch}}"])
            .map(|arch| arch.trim().to_string())
            .map_err(|error| error.to_string())
    }

    fn labels(&self, image: &str) -> Result<Option<BTreeMap<String, String>>, String> {
        // Existence first, so that "not there" is an answer and a daemon that cannot answer is not mistaken for
        // it: an image whose label read fails *after* the daemon said it exists is a machine problem, which is
        // an `Err` here and a sentence for the operator.
        if !Docker::image_exists(self, image) {
            return Ok(None);
        }
        Docker::image_inspect(self, image, "{{json .Config.Labels}}")
            .map(|text| Some(parse_labels(&text)))
            .map_err(|error| error.to_string())
    }

    fn build(&self, tag: &str, archive: &[u8], args: &[String]) -> Result<Log, String> {
        match Docker::build_bounded(self, args, archive, BUILD_TIMEOUT) {
            Ok(Some(log)) => Ok(log),
            Ok(None) => Err(format!(
                "the build of {tag} did not finish within {}s and was killed: a first build installs pi over \
                 the network, so this is a machine that could not reach it rather than a Dockerfile that is wrong",
                BUILD_TIMEOUT.as_secs()
            )),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// A `{{json .Config.Labels}}` answer as a map.
///
/// A `null` is an image with no labels — Docker's answer for a nil map — and it is an empty map here, so that
/// the caller has one shape to read rather than two.
fn parse_labels(text: &str) -> BTreeMap<String, String> {
    serde_json::from_str::<Option<BTreeMap<String, String>>>(text.trim())
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// The ustar archive the daemon reads as a build context: `Dockerfile` (0644) and `holosmith` (0755).
///
/// Written by hand rather than through a `tar` crate, because this is the whole of what the daemon needs and a
/// dependency for two files is not worth one. **`uid`, `gid` and `mtime` are zero**, so the same Dockerfile and
/// the same binary bytes always produce the same archive: a build that depended on when it ran would make every
/// rebuild a different context for the daemon's cache.
///
/// The binary is read here, so a path that is not a file fails with a sentence before the daemon is asked
/// anything: a build from an archive with a missing entry would report a `COPY` failure inside the image, which
/// names the wrong thing.
pub fn archive(dockerfile: &str, binary: &Path) -> Result<Vec<u8>, String> {
    let program = std::fs::read(binary).map_err(|error| {
        format!(
            "the binary at {} could not be read ({error}): the build context is an archive holding the \
             Dockerfile and this program, and an image built without it would have no holosmith in it",
            binary.display()
        )
    })?;
    Ok(archive_bytes(dockerfile, &program))
}

/// The same archive from bytes the caller already holds.
///
/// The smoke run's half (HS-077): it has read, hashed and ELF-checked the candidate before it builds anything, so a
/// second read could be a second file — and a promotion's image is supposed to be built from the bytes it measured.
pub fn archive_bytes(dockerfile: &str, program: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_entry(&mut out, "Dockerfile", 0o644, dockerfile.as_bytes());
    push_entry(&mut out, "holosmith", 0o755, program);
    // Two zero blocks end a tar archive: without them the daemon reads a truncated context.
    out.resize(out.len() + 1024, 0);
    out
}

/// One ustar entry: a 512-byte header, the data, and the zero padding that rounds it to a block.
///
/// `pub(crate)` since HS-076, whose builder archive is a second caller: the daemon reads a context it is handed, and
/// one ustar writer for the three images is one place the header can be wrong.
pub(crate) fn push_entry(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
    let mut header = [0_u8; 512];
    // The name is 100 bytes with a NUL; every name here is far shorter, and a longer one would be a `panic`
    // rather than an archive the daemon misreads.
    header[..name.len()].copy_from_slice(name.as_bytes());
    write_octal(&mut header[100..108], u64::from(mode));
    // uid, gid and mtime stay zero: see [`archive`] on why the same bytes must give the same archive.
    write_octal(&mut header[124..136], data.len() as u64);
    // The checksum is computed with its own field as eight spaces, which is the one field a tar header cannot
    // compute in the same pass as the others.
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(data);
    let padding = (512 - data.len() % 512) % 512;
    out.resize(out.len() + padding, 0);
}

/// One numeric header field: octal, zero-padded, with the NUL a ustar header ends it with.
///
/// The field's last byte is the NUL and the rest are digits, so an 8-byte field holds `%07o` — which is the
/// format ustar specifies and what every tar reads.
fn write_octal(field: &mut [u8], value: u64) {
    let width = field.len() - 1;
    let text = format!("{value:0width$o}");
    let bytes = text.as_bytes();
    field[..width].copy_from_slice(&bytes[bytes.len() - width..]);
    field[width] = 0;
}

/// The `docker build` argument vector, `-` last: the context is the archive on stdin.
///
/// `--build-arg` for every fact the Dockerfile labels with, in [`Expectation::build_args`]'s order, so the
/// image's labels are this binary's values and not a constant in a file.
pub fn build_arguments(tag: &str, expectation: &Expectation) -> Vec<String> {
    let mut arguments = vec!["build".to_string(), "-t".to_string(), tag.to_string()];
    for (key, value) in expectation.build_args() {
        arguments.push("--build-arg".to_string());
        arguments.push(format!("{key}={value}"));
    }
    arguments.push("-".to_string());
    arguments
}

/// The failing instruction and layer from a build's output, as one sentence.
///
/// BuildKit's failure prints the layer it stopped in (` > [2/2] RUN apk add …:`), the instruction it came from
/// with the whole Dockerfile around it (`   2 | >>> RUN apk add …`), and then the child's own last line. The
/// sentence keeps those three and drops the rest: the surrounding Dockerfile lines are the same in every
/// failure, and an operator pasting two hundred lines into a ticket has not been told which line is wrong.
///
/// Empty input still answers usefully: a daemon that said nothing is a build that failed without a reason, and
/// the sentence says so rather than starting with a colon.
pub fn failure(image: &str, text: &str) -> String {
    let layer = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("> "))
        .map(|rest| rest.trim().trim_end_matches(':').to_string())
        .filter(|layer| !layer.is_empty());
    let instruction = text
        .lines()
        .find_map(|line| {
            line.split_once(">>> ")
                .map(|(_, rest)| rest.trim().to_string())
        })
        .filter(|instruction| !instruction.is_empty());
    let last = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let last = clip(last, 300);
    let at = match (&layer, &instruction) {
        (Some(layer), Some(instruction)) => format!(" at {layer} ({instruction})"),
        (Some(layer), None) => format!(" at {layer}"),
        (None, Some(instruction)) => format!(" at `{instruction}`"),
        (None, None) => String::new(),
    };
    if last.is_empty() {
        return format!(
            "the build of {image} failed{at} and printed nothing, so no new image was tagged {image}"
        );
    }
    format!("the build of {image} failed{at}, and no new image was tagged {image}: {last}")
}

/// `text` cut to `limit` characters, with an ellipsis when it was longer.
///
/// Characters rather than bytes: a clipped sentence is a line an operator reads, and half a multi-byte character
/// is not one. The count is generous because a compile error is one line and a Docker error is a paragraph.
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…")
}

/// One image's state, as step 3's read found it.
///
/// The `String` in `Missing` and `Stale` is the **reason a build is needed**, in the words the line will print:
/// a read that answered `false` would leave the operator asking why a deploy wants to build an image that is
/// already there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Current,
    Missing(String),
    Stale(String),
}

/// Read one image's labels and compare them with `expectation`.
///
/// Every arm is an answer: a daemon that cannot answer becomes a `Missing`/`Stale` carrying what it said, because
/// a read that failed would stop the run at step 3 in a *dry run* — and a dry run's whole job is to print the
/// plan, including the plan for a machine it cannot fully measure. The `Result` this once had never carried an
/// `Err`, and a signature that promises an error nothing produces invites a caller's dead arm.
pub fn read(images: &dyn Images, kind: Kind, expectation: &Expectation) -> Status {
    let image = kind.image();
    match images.labels(&image) {
        Ok(Some(labels)) if expectation.unbuilt(&labels) => Status::Stale(format!(
            "{image} carries no Holosmith labels, so it was not built from this binary's Dockerfile"
        )),
        Ok(Some(labels)) => {
            let mismatches = expectation.mismatches(&labels);
            if mismatches.is_empty() {
                Status::Current
            } else {
                Status::Stale(format!(
                    "{image} does not match this binary: {}",
                    mismatches.join("; ")
                ))
            }
        }
        Ok(None) => Status::Missing(format!("{image} is not on this machine")),
        // There but unreadable is not "absent": the image is there, and what cannot be compared is a build.
        Err(said) => Status::Stale(format!(
            "{image} is there, but its labels could not be read ({said})"
        )),
    }
}

/// The architecture guard: `Ok(Docker's word)` when the daemon builds for this binary's target.
///
/// The daemon's architecture is not this binary's by definition. A `docker_host` may point at a socket on
/// another machine, and an image built there has a base for that machine: an `arm64` base with an `x86_64`
/// `holosmith` in it is an image whose entrypoint cannot start, and the failure would arrive as a proxy that
/// will not run rather than as a deploy that refused. So both are compared by their **architecture** words —
/// Docker's `amd64`/`arm64` and the target triple's `x86_64`/`aarch64` — before the first build, and anything
/// else is compared verbatim rather than guessed at.
pub fn architecture(images: &dyn Images, target: &str) -> Result<String, String> {
    // A daemon that cannot answer is the same failure as a mismatch and needs the same sentence's subject: the two
    // images nothing will build. Docker's own words about `docker version` name neither of them.
    let arch = images.daemon_arch().map_err(|said| {
        format!(
            "no image was built, because this machine's daemon did not say which architecture it builds for \
             ({said}): {} and {} are built here, and a build for another architecture is an image whose entrypoint \
             could not start",
            proxy_image(),
            agent_image()
        )
    })?;
    let binary = target.split('-').next().unwrap_or(target);
    let built = match arch.as_str() {
        "amd64" => "x86_64",
        "arm64" => "aarch64",
        other => other,
    };
    if built == binary {
        return Ok(arch);
    }
    Err(format!(
        "this binary is {binary} and this machine's daemon builds {arch}: {} and {} would be {arch} images with \
         a {binary} holosmith inside, whose entrypoint could not start — build this machine's artifact, or \
         deploy from the build for {built}",
        proxy_image(),
        agent_image()
    ))
}

/// Build the images `pending` names, proxy first, answering the line's sentence; `Err` is the step's failure.
///
/// The order is the spec's: the proxy image is what step 4 starts, so a machine that runs out of bounds or disk
/// has the image its proxies need before the larger one. The guard runs once, before the first build, because
/// both images are built by the same daemon and one mismatch is enough to stop.
pub fn ensure(
    images: &dyn Images,
    binary: &Path,
    digest: &str,
    config: &Config,
    pending: &[(Kind, String)],
) -> Result<String, String> {
    if pending.is_empty() {
        return Ok("no image needed building".to_string());
    }
    architecture(images, env!("HOLOSMITH_TARGET"))?;
    for (kind, _) in pending {
        let expectation = Expectation::of_digest(*kind, config, digest);
        let tag = kind.image();
        let context = archive(kind.dockerfile(), binary)?;
        let arguments = build_arguments(&tag, &expectation);
        match images.build(&tag, &context, &arguments)? {
            Log { success: true, .. } => {}
            Log {
                success: false,
                text,
            } => return Err(failure(&tag, &text)),
        }
    }
    let tags: Vec<String> = pending.iter().map(|(kind, _)| kind.image()).collect();
    Ok(format!(
        "built {} image{}: {}",
        tags.len(),
        if tags.len() == 1 { "" } else { "s" },
        tags.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_standards_vectors() {
        // FIPS 180-4's own examples — the empty message, one single-block message, and one whose padding crosses a
        // block boundary (which is what catches an off-by-one in the length field). The label's value is only as
        // trustworthy as this function, and an operator can check it with `sha256sum` on the artifact.
        for (input, wanted) in [
            (
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ] {
            assert_eq!(hex(&sha256(input.as_bytes())), wanted, "{input:?}");
        }
    }
}

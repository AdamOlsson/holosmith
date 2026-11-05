// The probe the startup self-test runs inside a container (HS-036).
//
// It is handed the addresses a session network must not reach and its verdict is its **exit code**, because
// that is the one thing `docker run` reports without parsing anything: 0 means nothing was reachable, 9 means
// something was. The stdout of the reached set is what the sentence names.
//
// **The distinction the whole test rests on is `connected`/`refused` versus everything else.** A connection
// that succeeds proves a route out; a connection the far side *refuses* proves the packet arrived and the host
// answered with a reset, which is a route to the host — on this machine's default bridge the host's own
// `172.17.0.1:2375` answers exactly that way, so a rule that only counted `connected` would pass a machine
// whose containers can reach the host. `ENETUNREACH`, `EHOSTUNREACH` and a timeout are the answers an internal
// network gives, and they are the pass.
//
// Node rather than `nc`/`curl`: the image sessions run has node and has neither of the others (measured), and a
// second image just for the probe would be a second thing a machine could be missing.
const net = require("net");

const CONNECTING = "connected";
const REFUSED = "refused";
const REACHED_EXIT = 9;

const targets = process.argv.slice(1);

/// One address, one verdict. A socket that errors after resolving is not asked again: the first answer is the
/// answer, which is what keeps a refused-once host from being retried into a timeout.
const attempt = (host, port) =>
  new Promise((resolve) => {
    const socket = net.connect({ host, port });
    let settled = false;
    const finish = (verdict) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      resolve(verdict);
    };
    socket.setTimeout(3000);
    socket.on("connect", () => finish(CONNECTING));
    socket.on("timeout", () => finish("timed out"));
    socket.on("error", (error) =>
      finish(error.code === "ECONNREFUSED" ? REFUSED : String(error.code || error.message)),
    );
    socket.on("close", () => finish("closed"));
  });

Promise.all(
  targets.map((target) => {
    const cut = target.lastIndexOf(":");
    return attempt(target.slice(0, cut), Number(target.slice(cut + 1))).then((verdict) => [
      target,
      verdict,
    ]);
  }),
).then((results) => {
  for (const [target, verdict] of results) {
    console.error(`${target} ${verdict}`);
  }
  const reached = results.filter(([, verdict]) => verdict === CONNECTING || verdict === REFUSED);
  if (reached.length > 0) {
    console.log(reached.map(([target, verdict]) => `${target} ${verdict}`).join(","));
    process.exit(REACHED_EXIT);
  }
  process.exit(0);
});

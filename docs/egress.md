# Egress: what a session may reach

Read this when an agent cannot install packages, or before you widen what sessions can reach.
[Back to README](../README.md).

**A session has no Internet by default.** `[egress] allow = []` means the agent reaches its provider through the key
proxy and nothing else, so `cargo build` or `npm install` fails. Add the hosts a project needs in
`holosmith config edit`, then restart:

```toml
[egress]
allow = ["crates.io", "index.crates.io", "static.crates.io", "registry.npmjs.org", "github.com"]
```

```bash
systemctl --user restart holosmith
```

- **A session can also be opened with Internet access for one turn of work**: the new-session form's Internet access
  toggle starts from `network_default`, which is `false` by default, and ticking it is enough for a session that needs
  cargo or npm.
- **Provider traffic does not use this list.** It goes through the key proxy, which is why no provider host belongs in
  `allow`. A tool's host (`mcp.linear.app`) needs no entry either: the MCP proxy reaches it over its own TLS
  connection, and joins a session's network only while the checkbox is on (ADR-0099).
- **Only allow-listed HTTPS is proxied.** The route out is the egress proxy (CONNECT/443); anything else is refused
  inside the session's own `--internal` network.

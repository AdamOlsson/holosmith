# Hub: several machines behind one page

Read this when one browser tab should drive sessions on more than one machine. [Back to README](../README.md).

One page, several machines: `holosmith install` on a `role = "hub"` config on the always-on box, then `holosmith hub
host add <name> <url>` there and `allowed_hubs = ["<hub node name>"]` on this one. A hub runs no containers, so it
needs neither Docker nor a provider key.

## The registry

`holosmith hub host` manages the machines a hub forwards to, and every subcommand is a call to this machine's own
`POST`/`GET`/`DELETE /api/hosts`, so the rules, their codes and their sentences are the route's:

```bash
holosmith hub host add <name> <url>     # register one machine, or update the row it already has
holosmith hub host list                 # print every row, by name
holosmith hub host remove <name>        # forget one machine
```

- **A name is a row's identity**: one path segment — lowercase letters, digits and `-`.
- **A url is the machine's control origin**: `http(s)`, a MagicDNS name or an address in `100.64.0.0/10`, and a port.
- **A row that names this machine is refused while it has live sessions**, unless `--force` is given.

A hub serves the app, the host registry, and forwards API and preview requests; a server runs sessions and owns the
session API. Every role answers the same paths.

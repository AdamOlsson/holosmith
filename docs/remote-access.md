# Remote access: your phone, and the tailnet

Read this to drive the machine from a phone or another computer: the LAN path, the tailnet path, and where TLS comes
from. [Back to README](../README.md).

## From your phone, on the same wifi

Listen beyond loopback and answer the address the phone types (`ip -4 -o addr show scope global`), in
`holosmith config edit`, then `systemctl --user restart holosmith`:

```toml
control_listen = "0.0.0.0:8080"
preview_listen = "0.0.0.0:8081"
control_origin = "http://192.168.1.42:8080"
preview_origin = "http://192.168.1.42:8081"
allowed_hosts  = ["192.168.1.42"]
```

Open `http://192.168.1.42:8080` on the phone, and open 8080 (and 8081, for previews) on the firewall if there is one.

- **Only `control_origin` may change anything, and it holds one address**, port included; `allowed_hosts` is a list and
  `localhost` always loads. A new LAN address is therefore an edit plus a restart — give the machine a DHCP reservation,
  or use the name it answers on, rather than chasing the address.
- **Plain HTTP on the wifi is not a secure context**: no installable app, no push notifications, and narrower rules for
  browser storage and the clipboard. The tailnet section is how to get HTTPS.
- **Anyone on the wifi who types the address can drive every session.** The host and origin checks stop another web
  page, not a person; the tailnet is how to stay off the shared network.

## Over a tailnet

`tailscale serve` is the operator's own step — the listener is your process, the tailnet is root's. The app is served
on **443** and previews on **8444** on the node's name, so the origins are that name.

1. `tailscale up` if this node is not logged in; `tailscale status --json | jq -r .Self.DNSName` gives the name
   (`laptop.tailnet.ts.net.`, trailing dot dropped).
2. Put it in the config first (`holosmith config edit`) — without it the tailnet page loads and every action is
   refused. The listeners stay on loopback, because that is where `tailscale serve` connects from:

   ```toml
   control_origin = "https://laptop.tailnet.ts.net"
   preview_origin = "https://laptop.tailnet.ts.net:8444"
   allowed_hosts  = ["laptop.tailnet.ts.net"]
   ```

3. `systemctl --user restart holosmith` — the new origins are read at boot, and a deploy does not restart a unit that
   is already enabled. Then forward both ports; both entries survive a reboot:

   ```bash
   sudo tailscale serve --bg --yes --https=443  http://127.0.0.1:8080
   sudo tailscale serve --bg --yes --https=8444 http://127.0.0.1:8081
   ```

4. Open `https://laptop.tailnet.ts.net` from any device on the tailnet.

**Headscale issues no certificates**, so `--https` fails there: serve `--http=8080` and `--http=8081` instead, put the
matching `:8080` and `:8081` origins and name in `allowed_hosts`, and accept plain HTTP's cost.

## A certificate of your own

The third way is to terminate TLS in front of `127.0.0.1:8080` and point the origins at that name — `docs/config.md`
has the block. Both listeners stay on loopback; only the proxy in front is reachable from outside.

## Optional hardening

`owner = "<your tailnet login>"` makes every request require that login, which `tailscale serve` supplies as a header.
Direct LAN access carries no header and is unaffected.

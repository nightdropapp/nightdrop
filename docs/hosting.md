# Hosting the website

Night Drop is published from one `website/` directory to **two** places:

| | Serves | Runs on | Deployed by |
|---|---|---|---|
| **Onion site** | `z6xw2y…qqd.onion` | the maintainer's machine, `nightdrop-onion.service` (nginx + tor, `scripts/onion-website.sh`) | writing into `website/` — it serves live from disk |
| **Clear web** | `https://nightdrop.app` | the same machine, system nginx (`/etc/nginx/conf.d/nightdrop.conf` = `deploy/nginx-nightdrop.conf`) | `scripts/deploy-clearnet.sh` |

The clear-web site is marketing and contact metadata. The onion additionally serves the **release
binaries** and `update.json`, and is the app's only update channel (`core/src/update.rs`,
`ARCHITECTURE.md` §10a). Losing the clear-web host costs a landing page; losing the onion address
breaks updates for every installed build (§6).

> **Everything runs on one machine.** A reboot here takes down the landing page, the onion site
> and the relay together. Moving them somewhere with real uptime is a standing improvement (the
> Proxmox plan in the maintainer's TODO), not an emergency.

Publishing procedure and its traps: `MAINTENANCE.md` §11–§12.

## 1. The clear-web setup (since 2026-09-12)

- **DNS** (Namecheap): `nightdrop.app` and `www` → the machine's static public address (in
  `MAINTENANCE.local.md`).
- **Router**: a NAT rule forwards WAN 80/443 to the machine; router management is not reachable
  from the internet. Addresses and rule names are in the gitignored `MAINTENANCE.local.md`.
- **nginx**: Fedora's system nginx (1.30). Port 443 is shared with other sites on the box, so
  switching what a hostname serves is an nginx include, never a router change.
- **Web root**: `/var/www/nightdrop`, owned by the maintainer, so `scripts/deploy-clearnet.sh` needs
  no sudo. Files must carry `httpd_sys_content_t` (rsync without `-X/-A` lets them inherit it).
- **TLS**: one Let's Encrypt certificate for apex + www, renewed by `certbot-renew.timer` using the
  nginx authenticator. Check the expiry with
  `echo | openssl s_client -connect 127.0.0.1:443 -servername nightdrop.app | openssl x509 -noout -enddate`.
- **SELinux**: serving static files needs nothing extra. *Proxying* to a local port does: system
  nginx runs as `httpd_t`, `httpd_can_network_connect` is off, so every proxied backend port must be
  labelled `http_port_t`; on a 502, read `journalctl | grep 'SELinux is preventing'`. A rehearsal
  under a user-launched nginx (`unconfined_t`) will not show this.

### Testing from outside

Probing the public IP from this machine only tests hairpin NAT, and remote fetchers can report
connection refused for ports the router provably forwards. Test from a phone on **mobile data**,
and watch `ss -tn` here while it connects.

## 2. One-time: the vhost

```sh
sudo install -d -o "$USER" -g "$USER" /var/www/nightdrop
sudo certbot certonly --nginx -d nightdrop.app -d www.nightdrop.app   # BEFORE the vhost
sudo cp deploy/nginx-nightdrop.conf /etc/nginx/conf.d/nightdrop.conf
sudo nginx -t && sudo systemctl reload nginx
scripts/deploy-clearnet.sh
```

`certonly` keeps certbot from rewriting a config shared with other sites, and the vhost fails
`nginx -t` until the certificate exists.

The onion vhost lives in a **separate** file, `deploy/nginx-nightdrop-onion.conf`, so it cannot be
installed by accident. Do not install it unless you are migrating the onion (§6).

Details in `deploy/nginx-nightdrop.conf` that look wrong and are not:

- `location = /SECURITY.md` uses `types { } default_type text/plain`. The empty block is
  load-bearing: nginx ≥ 1.21 maps `.md` to `text/markdown`, and `default_type` applies only to
  *unmapped* extensions. Without it the policy downloads instead of rendering.
- `http2 on;` needs nginx ≥ 1.25.1. On an older nginx (Ubuntu 20.04–24.04 ship 1.18–1.24) it fails
  with `unknown directive "http2"`; use `listen 443 ssl http2;` there instead.
- No `types { … webmanifest; }` block: nginx ≥ 1.21's `mime.types` maps it already.

Check the `Onion-Location` header still matches the live address in
`~/.local/share/nightdrop-website-onion/hs/hostname` and in `core/src/update.rs`. All three must agree.

## 3. Deploying

```sh
scripts/deploy-clearnet.sh        # web root defaults to /var/www/nightdrop
```

Idempotent; run it after any change to `website/`. It regenerates `config.js` from
`config/app_config.json`, stages `SECURITY.md` into the web root, and rsyncs with `--delete`.
nginx serves static files, so no reload is needed. Two exclusions are deliberate:

- **`applications/`** — the APKs, AppImage and installer, which only the onion links to
  (`website/index.html` switches to same-origin `/applications/` paths only on a `.onion`
  hostname; on clear web `config.js` points at GitHub Releases). Publishing them here would also
  create a second download path that is not the signed one users are told to verify.
- **`README.md`** — developer documentation, and `robots.txt` admits every crawler.

`website/SECURITY.md` is **generated** (copied from the repo root at deploy time) and gitignored.
Do not commit a second copy: `security.txt` advertises `Policy: https://nightdrop.app/SECURITY.md`,
and a duplicate that drifts publishes a security policy disagreeing with the real one.

## 4. Verifying a deploy

```sh
curl -sSI https://nightdrop.app/ | head -3                       # 200, and the right cert
curl -sS  https://nightdrop.app/.well-known/security.txt | head  # clearsigned block, not 404
curl -sSI https://nightdrop.app/SECURITY.md | grep -i content-   # text/plain; charset=utf-8
curl -sSI https://nightdrop.app/ | grep -i onion-location        # matches the live .onion
curl -s   https://nightdrop.app/update.json | head -c 40         # the current release
```

Then open it in Tor Browser and confirm the Onion-Location banner offers the onion site.

## 5. The onion site

`scripts/onion-website.sh` runs nginx on localhost behind a tor v3 onion service, with a watchdog
that dials the onion end to end (its header explains why nothing less is trusted). The onion key
is in `~/.local/share/nightdrop-website-onion/` (keep it private and backed up); install as a user
service with `scripts/install-onion-service.sh`. Local check: `curl -s http://127.0.0.1:8787/`.

## 6. Before moving the onion to another host — read this

`core/src/update.rs` hardcodes `z6xw2y…qqd.onion` as the app's **only** update channel: no
clearnet fallback, by design (a v3 onion address is an ed25519 public key, so reaching it
authenticates it). Every build already in users' hands depends on that address staying reachable.

So a migration is a one-shot key move, and the two ways to get it wrong are both permanent:

- **Losing `hs_ed25519_secret_key` means a new address**, and there is no way to tell existing
  installs about it — they check one address forever. Back it up before touching anything.
- **Never publish the same key from two hosts.** Both tor instances publish descriptors for the
  same address and clients get whichever landed last, which is worse than either host alone. Stop
  `nightdrop-onion.service` here *before* starting tor on the new host.

`deploy/onion-torrc` has the file-by-file procedure (the tor user is `debian-tor` on Debian/Ubuntu,
`toranon` on Fedora). A restart rotates introduction points and clients keep using the old
descriptor until they refetch, so a cutover is not instantaneous even when done correctly. Do it as
its own deliberate operation, never bundled with a clear-web change.

## 7. Open items needing a human

- [ ] Submit `sitemap.xml` to Google Search Console and Bing Webmaster Tools.
- [ ] Optional: a CAA record pinning `letsencrypt.org` (there is none today).

Inbound mail to `security@nightdrop.app` (Proton Mail on the custom domain) is confirmed working:
an outside sender's message arrived on 2026-10-06.

## 8. History

- **Until 2026-09-12** the clear web was planned for, then served from, a VPS (162.247.131.86,
  nginx 1.18 on Ubuntu 20.04, shared with a mail host) via a `deploy-vps.sh` script. The VPS has
  since been shut down and the script removed.
- **Until the 2026-09-12 cutover** the domain served an unrelated placeholder site from this
  machine; its vhost is parked (disabled) and the old `/var/www/nightdrop.app` web root is unused.
  The cutover was rehearsed under a user-level nginx (14/14 checks) and verified live (11/11, a phone on
  LTE, `certbot renew --dry-run`).
- **Before the first clear-web deploy** (2026-09-01) `SECURITY.md`'s "before this is live" block
  was removed and `security.txt` re-clearsigned (`docs/security-txt.md`): both now publish the
  security address as a monitored inbox.

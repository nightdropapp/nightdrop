# Hosting the clear-web site

Night Drop is published from **two independent places**, and keeping them straight is the
whole point of this document:

| | Serves | Runs on | Deployed by |
|---|---|---|---|
| **Clear web** | `https://nightdrop.app` | **this machine**, system nginx (`/etc/nginx/conf.d/nightdrop.conf`) | `~/nightdrop-clearnet-cutover/deploy-local.sh` |
| **Onion mirror** | `z6xw2y…qqd.onion` | this machine, `nightdrop-onion.service` | writing into `website/` — it serves live from disk |

> **Both sites now run on this one machine.** That is the opposite of the "two independent
> places" this document was written to describe, and it is worth saying plainly: a reboot here
> takes down the landing page, the onion mirror, and the relay together. Moving any of them
> somewhere with real uptime is a standing improvement, not an emergency.

> **Correction, 2026-09-21.** Everything below §1 describes the **old VPS** (162.247.131.86) and
> is kept as history. The clear web was cut over to this machine on **2026-09-12**; the VPS has
> since been shut down. Verified today: system nginx active, `nightdrop.conf` present,
> `https://nightdrop.app/` answering 200 locally, DNS A → **69.72.55.130** (the static address on
> the MikroTik's `pppoe-wan`, forwarded to 192.168.88.94).
>
> Two consequences that differ from the VPS era:
> - **Deploy with `~/nightdrop-clearnet-cutover/deploy-local.sh`, not `scripts/deploy-vps.sh`.**
>   It needs no sudo, and it **excludes `applications/` by design** — clearnet downloads are
>   served from GitHub Releases via `config.js`, so binaries stay onion-only.
> - The certificate covers apex + www via certbot's **nginx** authenticator and **expires
>   2026-10-25**, so web-root moves do not affect renewal but the date is worth a diary entry.

They share the `website/` directory as a source and nothing else. The clear-web site is
marketing and contact metadata; the onion additionally serves the **release binaries** and
`update.json`, and is the app's only update channel (`core/src/update.rs`). Losing the
clear-web host costs a landing page. Losing the onion address breaks updates for every
installed build — see §6.

## 1. The server, as verified 2026-09-01 — HISTORICAL, see the correction above

- `nightdrop.app` and `www.nightdrop.app` both resolve to **162.247.131.86**. DNS is at
  Namecheap (`dns1/dns2.registrar-servers.com`).
- That host ran **nginx/1.18.0 on Ubuntu 20.04** and also terminated TLS for
  `mail.shawnbourgeois.xyz`. Treat it as shared: add a vhost in `conf.d`, never rewrite
  `nginx.conf`. (20.04 went out of standard support in April 2025; an OS upgrade or rebuild was
  planned as of this writing, so re-check the version before trusting this line.)
- There is no certificate for `nightdrop.app` yet; `https://nightdrop.app` currently answers
  with the mail host's cert and fails verification.
- Mail is on Proton Mail (`MX 10 mail.protonmail.ch`, SPF present), so `security@nightdrop.app`
  is plausibly already deliverable — confirm by sending to it before relying on it.

## 2. One-time: the nginx vhost

`deploy/nginx-nightdrop.conf` is drop-in — it contains the clear-web vhost only, and needs no
editing:

```sh
sudo install -d -o "$USER" -g "$USER" /var/www/nightdrop
sudo cp deploy/nginx-nightdrop.conf /etc/nginx/conf.d/nightdrop.conf
sudo nginx -t && sudo systemctl reload nginx
```

Obtain the certificate first (§3) — the vhost references `/etc/letsencrypt/live/nightdrop.app/`
and `nginx -t` fails until it exists.

The onion vhost lives in a **separate** file, `deploy/nginx-nightdrop-onion.conf`, specifically
so it cannot be installed by accident. Do not install it unless you have migrated the onion (§6).

Two details in the clear-web file that look wrong and are not:

- The top-level `types { … webmanifest; }` block **merges** with the inherited
  `include mime.types` rather than replacing it, so it cannot break MIME types for the other
  vhosts on this box. Verified directly: with both present, `.css` still resolves to `text/css`
  and `.webmanifest` to `application/manifest+json`.
- `location = /SECURITY.md` uses `types { } default_type text/plain`. The empty block is
  load-bearing: nginx ≥ 1.21 maps `.md` to `text/markdown` in `mime.types` (1.18 has no markdown
  entry at all), and `default_type` applies only to *unmapped* extensions. Without it the policy
  downloads instead of rendering on a newer nginx — and the naive fix would appear to work on
  1.18 and silently regress on the next upgrade.
- HTTP/2 is a `listen` parameter, not the `http2 on;` directive. This is not stylistic: `http2 on;`
  did not exist before nginx 1.25.1 and fails there outright with `unknown directive "http2"`,
  which is how the first install attempt broke.

### nginx version matrix

Verified by running this exact vhost under each version, because the whole file has now been
wrong once on a version assumption:

| Ubuntu | nginx | `listen … http2` | `http2 on;` |
|---|---|---|---|
| 20.04 | 1.18.0 | required | `unknown directive` |
| 22.04 | 1.18.0 | required | `unknown directive` |
| 24.04 | 1.24.0 | required | `unknown directive` |
| 26.04 | 1.28.3 | deprecation **warning**, still works | preferred |

So the committed spelling is valid on every release from 20.04 to 26.04 and no upgrade forces a
config change. Switch to `http2 on;` only after the server is on nginx ≥ 1.25.1, and only to
silence the warning.

Check the `Onion-Location` header still matches the live address in
`~/.local/share/nightdrop-website-onion/hs/hostname` and in `core/src/update.rs`. All three must
agree; they did as of 2026-09-01.

## 3. One-time: TLS

Obtain the certificate **before** installing the vhost, which otherwise fails `nginx -t` on the
missing `ssl_certificate`. Use `certonly` so certbot does not rewrite the config on a box that is
also serving mail — it adds a temporary server block for the challenge and removes it again:

```sh
sudo certbot certonly --nginx -d nightdrop.app -d www.nightdrop.app
```

DNS already points here, so the challenge passes immediately. Then install the vhost (§2).
Certbot's renewal timer will keep it current; the vhost needs no further changes.

Optional hardening once issued: a CAA record pinning `letsencrypt.org` (there is none today).

## 4. Deploying

```sh
scripts/deploy-vps.sh user@162.247.131.86        # web root defaults to /var/www/nightdrop
```

Idempotent; run it after any change to `website/`. It regenerates `config.js` from
`config/app_config.json`, stages `SECURITY.md` into the web root, rsyncs with `--delete`, and
reloads nginx over SSH. The SSH user needs write access to the web root and
`sudo systemctl reload nginx`; without the latter the script warns and you reload by hand.

Two exclusions are deliberate:

- **`applications/`** — ~260 MB of APKs and AppImages that only the onion mirror links to
  (`website/index.html` switches to same-origin `/applications/` paths only when the hostname
  ends in `.onion`; on clear web `config.js` points at GitHub Releases). Publishing them here
  would also create a second download path that is not the signed one users are told to verify.
- **`README.md`** — developer documentation, and `robots.txt` admits every crawler.

`website/SECURITY.md` is **generated** (copied from the repo root at deploy time) and
gitignored. Do not commit a second copy: `security.txt` advertises
`Policy: https://nightdrop.app/SECURITY.md`, and a duplicate that drifts publishes a security
policy disagreeing with the real one.

## 5. Verifying a deploy

```sh
curl -sSI https://nightdrop.app/ | head -3                       # 200, and the right cert
curl -sS  https://nightdrop.app/.well-known/security.txt | head  # clearsigned block, not 404
curl -sSI https://nightdrop.app/SECURITY.md | grep -i content-   # text/plain; charset=utf-8
curl -sSI https://nightdrop.app/ | grep -i onion-location        # matches the live .onion
```

Then open it in Tor Browser and confirm the Onion-Location banner offers the mirror.

## 6. Before migrating the onion to this host — read this

`core/src/update.rs` hardcodes `z6xw2y…qqd.onion` as the app's **only** update channel: no
clearnet fallback, by design (a v3 onion address is an ed25519 public key, so reaching it
authenticates it). Every build already in users' hands depends on that address staying
reachable.

So a migration is a one-shot key move, and the two ways to get it wrong are both permanent:

- **Losing `hs_ed25519_secret_key` means a new address**, and there is no way to tell existing
  installs about it — they check one address forever. Back it up before touching anything.
- **Never publish the same key from two hosts.** Both tor instances will publish descriptors
  for the same address and clients get whichever landed last, which is worse than either host
  alone. Stop `nightdrop-onion.service` on the laptop *before* starting tor on the VPS.

`deploy/onion-torrc` has the file-by-file procedure. Note that on Ubuntu the tor user is
`debian-tor`, not Fedora's `toranon`.

Also remember that a restart rotates introduction points and clients keep using the old
descriptor until they refetch, so the cutover is not instantaneous even when done correctly.

The argument *for* migrating is real: the laptop-hosted onion goes dark for unexplained
multi-minute windows (documented at length in `scripts/onion-website.sh`), and an always-on
host with an independent probe would both reduce that and let the watchdog distinguish "our
tor cannot dial it" from "it is unreachable". Do it as its own deliberate operation, never
bundled with a clear-web change.

## 7. Go-live checklist

Repo-side, done 2026-09-01 — recorded here so a later reader knows these were deliberate:

- [x] `SECURITY.md` — the `> **Before this is live:**` block removed. The file is now *published*
      (§4), so the served policy would otherwise have told readers its own contact channel was
      unreachable.
- [x] `website/.well-known/security.txt` — the same note dropped from its comment header and the
      file **re-clearsigned** with the security key (`gpg --verify` passes). See
      `docs/security-txt.md`; any future edit needs the same treatment.
- [x] `website/sitemap.xml` — `lastmod` refreshed.
- [x] `website/index.html` — a platform with no download configured is now omitted rather than
      rendered as a dead `#` button. Windows had no build and no URL, so it was shipping a
      download button that went nowhere.
- [x] `website/README.md` — the stale "Before first release" list replaced; every item in it was
      already satisfied.

Still needs a human:

- [ ] **Send a test mail to `security@nightdrop.app` and confirm it arrives.** DNS says Proton
      Mail with SPF (§1), which is good evidence but not proof of delivery — and `SECURITY.md`
      and `security.txt` now both publish it as a monitored inbox with no hedge.
- [ ] After the first successful deploy, submit `sitemap.xml` to Google Search Console and Bing
      Webmaster Tools.
- [ ] Optional: a CAA record pinning `letsencrypt.org` (there is none today).

## 8. What was rehearsed locally, and what was not

Before the first deploy the clear-web vhost was run against a realistic web root (a real
`rsync` with the production excludes, plus the staged `SECURITY.md`) under a local nginx with a
stand-in certificate. Confirmed there: `/` 200; `/SECURITY.md` as `text/plain; charset=utf-8`;
`/.well-known/security.txt` as `text/plain`; `/manifest.webmanifest` as
`application/manifest+json`; `/styles.css` still `text/css`; `/applications/` a 404; the
`Onion-Location` header matching the live onion; and HTTP → HTTPS returning 301.

That covers the config and the content. It does **not** cover the things only the real host can
show: certificate issuance, coexistence with the mail vhost already on that box, and DNS. Check
those with §5 against the live site after deploying.

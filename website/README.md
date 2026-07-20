# Night Drop — website

A static marketing/features site for Night Drop. No build step or framework — just open
`index.html` (or serve the folder).

```sh
# Preview locally (from the repo root; binds to 127.0.0.1 only)
python3 -m http.server --bind 127.0.0.1 --directory website 8000
# then open http://localhost:8000
```

It explains the app's privacy strengths **without naming competitors**, and hosts the
donation addresses and download links.

## Serve over Tor (.onion)

To test the site as a Tor onion service (localhost-only HTTP behind a v3 hidden service):

```sh
sudo dnf install -y tor          # one-time: the tor daemon
scripts/onion-website.sh         # prints the http://<...>.onion address; Ctrl-C to stop
```

Open the printed `.onion` in Tor Browser (`sudo dnf install -y torbrowser-launcher`, then
run `torbrowser-launcher`). The onion key persists in
`~/.local/share/nightdrop-website-onion` so the address is stable across restarts — keep
that directory private.

To keep it running across reboots, install it as a systemd **user** service:

```sh
scripts/install-onion-service.sh   # enables + starts nightdrop-onion.service
```

Manage it with `systemctl --user {status,restart,disable} nightdrop-onion` and read the
`.onion` from `journalctl --user -u nightdrop-onion | grep onion`. Boot-without-login needs
lingering (`loginctl enable-linger $USER`); the installer enables it if it can.

## Single source of truth

Donation addresses, hero/donate copy, and download links come from the repo-root
**`config/app_config.json`** — the one place to edit them for **both** the website and the
app. After editing, run:

```sh
make config
```

That regenerates `website/config.js` (a `window.NIGHTDROP_CONFIG = {…}` snapshot) and
`app/assets/app_config.json`. `index.html` loads `config.js` and fills in the
addresses/copy/links; if `config.js` is missing it falls back to the static placeholders in
the HTML, so the page still renders.

- `index.html` — page content + a small script that hydrates from `config.js`.
- `styles.css` — styling.
- `config.js` — **generated** (do not edit by hand).
- `icon-192.png`, `icon-512.png`, `icon-512-maskable.png`, `apple-touch-icon.png` —
  favicons / PWA icons, copied from the app's icon set (`images/IconKitchen-Output/web/`).

## SEO / crawlers / PWA

The pages carry the usual discoverability metadata: `<title>` + `meta description`,
`canonical`, Open Graph + Twitter card tags (for link previews), `theme-color`,
`referrer: no-referrer` (privacy), and JSON-LD structured data (`WebSite` +
`SoftwareApplication`). Supporting files:

- `robots.txt` — allows all crawlers and points at the sitemap.
- `sitemap.xml` — lists the two pages; bump `lastmod` when content changes.
- `manifest.webmanifest` — PWA install metadata (name, icons, colors), linked from both pages.

**The production domain `https://nightdrop.app` is baked into these** (canonical, `og:url`,
sitemap `<loc>`, the `Sitemap:` line in `robots.txt`, and `security.txt`). If the domain
changes, update all of those. After a real deploy, submit `sitemap.xml` in Google Search
Console / Bing Webmaster Tools.

## Before first release

- Replace the placeholder Zcash address in `config/app_config.json` (then `make config`).
- Fill `downloads` and `links.source` in `config/app_config.json` — the download
  buttons render as dead `#` links until then.
- Confirm the domain (`nightdrop.app`) and refresh `sitemap.xml` `lastmod`.

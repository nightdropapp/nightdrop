# Hosting Night Drop: VPS, clear web, onion, and email

This is a practical plan for running the three public surfaces — the marketing site
(`nightdrop.app`), its onion mirror, and email for `security@nightdrop.app` — on one VPS.
The deploy artifacts referenced here live in `deploy/` and `scripts/`.

## What actually has to run

| Surface | Software | Notes |
|---|---|---|
| Clear web | nginx serving static files | `deploy/nginx-nightdrop.conf`; TLS via Let's Encrypt. |
| Onion mirror | system `tor` + nginx localhost vhost | `deploy/onion-torrc`; reuse the existing `.onion` key. |
| Email | a mail stack **or** a managed provider | see [Email](#email) — this is the hard part. |

The site is tiny and static, so almost any VPS runs it. **Email is the constraint** that
should drive the VPS choice: sending mail needs outbound **port 25 unblocked**, control of
**reverse DNS (PTR)**, and a **clean IP** that isn't already on spam blocklists.

## VPS recommendation

Optimize for: privacy-friendly provider/jurisdiction, email-capable (port 25 + PTR + decent
IP reputation), crypto payment (you already take privacy-coin donations), and it must allow
running a Tor onion service (all do — an onion service is a Tor *client*, not a relay/exit).

**Primary — [1984 Hosting](https://1984.hosting) (Iceland).** Privacy-forward provider and
jurisdiction, green power, gives PTR control, allows self-hosted mail, accepts crypto. A
strong all-rounder for a privacy product; slightly pricier than budget hosts.

**Value alternative — [BuyVM / Frantech](https://buyvm.net) (Luxembourg or Las Vegas).**
Cheap, crypto-friendly, unblocks port 25 on request, PTR control, optional cheap DDoS
protection and block storage. Great price/performance; less of a "privacy brand" than 1984.

**Maximum anonymity — [Njalla](https://njal.la).** They register/own the resource on your
behalf so your name never appears; accepts Monero; also does domains. Best if anonymous
*ownership* matters most. Self-managed mail is possible but you do the work; pricier.

**Cheap + reliable, if anonymity is secondary — Hetzner.** Excellent hardware/price and
rock-solid, **but**: outbound port 25 is blocked until you request an unblock (and they may
decline for new/unverified accounts), abuse handling is strict, and signup wants identity.
Fine for the site + onion; weaker fit for anonymous self-hosted email.

**Rule of thumb:** if you want everything (incl. email) on one privacy-aligned box that takes
crypto, pick **1984** or **BuyVM**. Register the domain somewhere that supports
DNSSEC + privacy (Njalla, or a registrar with WHOIS privacy). Get the **smallest box that
comfortably runs a mail stack** — email (esp. Mailcow) wants ~2 vCPU / 4 GB RAM; the site and
onion are rounding error on top.

## Email

**Decision: Proton Mail on the `nightdrop.app` custom domain, and keep our own PGP key.**
Proton is only the mailbox — the security-contact inbox mostly receives **PGP-encrypted**
reports that are encrypted to the key published in `pgp.txt`, so Proton relays ciphertext it
cannot read and we decrypt locally with the offline private key. This keeps the highest-risk,
highest-effort piece (a public SMTP/IMAP surface, deliverability, blocklists, patching) off our
plate while changing **nothing** that's already published:

- `website/.well-known/security.txt`, `website/pgp.txt`, and `SECURITY.md` stay **as-is**
  (contact `security@nightdrop.app`, fingerprint `079B A016 9201 A8AB 11F3 2385 884E ACB8 89D0 2002`).
- The **private key never touches Proton** — it is not uploaded, and Proton is never asked to
  decrypt. It stays on the offline machine that generated it.

Because reports are E2E-encrypted to our key, Proton (or any provider) only ever sees an opaque
blob — the same trust posture as the app itself.

### Set up the domain in Proton
Proton Mail Plus (or Business) → **Settings → Domain names → Add domain `nightdrop.app`**.
Proton shows the exact records to add at your DNS host; they take this shape (the DKIM targets
and the verification code are **unique per domain — copy them from the Proton panel**, don't
guess):

```
# Verify ownership (unique code from the Proton panel)
nightdrop.app.                          TXT    "protonmail-verification=<code>"

# MX — route inbound mail to Proton
nightdrop.app.                          MX 10  mail.protonmail.ch.
nightdrop.app.                          MX 20  mailsec.protonmail.ch.

# SPF — authorize Proton to send for the domain
nightdrop.app.                          TXT    "v=spf1 include:_spf.protonmail.ch ~all"

# DKIM — 3 CNAMEs; targets are unique per domain (from the Proton panel)
protonmail._domainkey.nightdrop.app.    CNAME  protonmail.domainkey.<...>.domains.proton.ch.
protonmail2._domainkey.nightdrop.app.   CNAME  protonmail2.domainkey.<...>.domains.proton.ch.
protonmail3._domainkey.nightdrop.app.   CNAME  protonmail3.domainkey.<...>.domains.proton.ch.

# DMARC — start at quarantine + reports, tighten to reject once clean
_dmarc.nightdrop.app.                   TXT    "v=DMARC1; p=quarantine; rua=mailto:security@nightdrop.app"
```

Then create the address **`security@nightdrop.app`** in Proton. You do **not** upload our PGP
key to Proton and you do **not** enable Proton's address-key encryption for external senders —
we want reporters using the key from `pgp.txt`, not a Proton-held key. Verify delivery with
[MXToolbox](https://mxtoolbox.com) (SPF/DKIM/DMARC all passing).

### Read a PGP-encrypted report (keep-your-own-key flow)
Reporters follow `security.txt`, fetch `pgp.txt`, and encrypt to our key. Proton receives that
as a block it **cannot** open, so decrypt it locally:

- **Manual (zero setup):** in Proton, copy the `-----BEGIN PGP MESSAGE-----` … `END PGP MESSAGE`
  block out of the email and pipe it to your offline key:
  ```
  gpg --decrypt report.asc      # or: pbpaste | gpg --decrypt
  ```
- **Smoother (client integration):** run **[Proton Bridge](https://proton.me/mail/bridge)**
  (needs a paid plan) to expose Proton over local IMAP/SMTP, then point **Thunderbird** (import
  the private key into its OpenPGP keyring) or **mutt + GnuPG** at Bridge and let the client
  decrypt inline.

### Steer senders to our key (optional, recommended)
Since we host `nightdrop.app` ourselves, publish our public key via **WKD** so PGP clients that
auto-discover keys pick *ours* rather than any address key Proton advertises. Serve the armored
key (and the policy file) under `https://nightdrop.app/.well-known/openpgpkey/…`. `security.txt`'s
`Encryption:` line already points reporters at `pgp.txt`, so this is belt-and-suspenders, not
required.

### If we ever change our mind (self-host)
Full sovereignty means running **[Mailcow: dockerized](https://mailcow.email)** (Postfix,
Dovecot, Rspamd, DKIM, ACME, admin UI) — or lighter **Mailu** / turnkey **Mail-in-a-Box** — on
a VPS whose provider does **not** block port 25 and lets you set a **PTR** record (rules out
Hetzner; see the provider table above). You'd then own SPF/`v=spf1 mx -all`, a self-generated
DKIM key, DMARC, reverse DNS, and MTA-STS/TLS-RPT, and test to a 10/10 on
[mail-tester.com](https://www.mail-tester.com). This is the higher-effort path we're
deliberately **not** taking.

## Migrating the site + onion to the VPS

1. **Provision** the VPS, point DNS `A`/`AAAA` for `nightdrop.app` (+ `www`) at it, set PTR.
2. **nginx + TLS:** install nginx, copy `deploy/nginx-nightdrop.conf` to
   `/etc/nginx/conf.d/nightdrop.conf`, then `certbot --nginx -d nightdrop.app -d www.nightdrop.app`.
3. **Deploy the site:** from your laptop, `scripts/deploy-vps.sh user@nightdrop.app`
   (rsyncs `website/` → `/var/www/nightdrop`, reloads nginx). Re-run on every change.
4. **Onion:** `sudo dnf install -y tor` (or `apt install tor`), append `deploy/onion-torrc`
   to the tor config, and **migrate the existing onion key** so the address stays the same
   (`z6xw2ywlybjeskki4jons5ujepc2pedp5qkgvtbyxorie46qnjpnzqqd.onion`): copy the three files
   from `~/.local/share/nightdrop-website-onion/hs/` into the server's `HiddenServiceDir`
   (see `deploy/onion-torrc` for exact perms/owner), over scp, then `systemctl reload tor`.
   The `Onion-Location` header in the nginx config advertises the mirror to Tor Browser.
5. **Email:** add the `nightdrop.app` domain in Proton and set the MX/SPF/DKIM/DMARC records
   from the Email section (no mail software runs on the VPS — Proton is the mailbox). Reports
   stay encrypted to our `pgp.txt` key and are decrypted locally.
6. **security.txt is already correct** — it points at `nightdrop.app` and `security@nightdrop.app`
   and is clearsigned; it deploys with the site. Just refresh `Expires` before it lapses
   (`docs/security-txt.md`).

## Hardening checklist

- SSH: keys only (`PasswordAuthentication no`), non-root deploy user, firewall (nginx: 80/443;
  **not** 8080/8787 — those stay localhost). No mail ports needed with Proton (only 25/465/587/993
  if you ever take the self-host path).
- Unattended security updates; fail2ban on ssh.
- Keep the onion `HiddenServiceDir` `0700` and its secret key backed up offline (it *is* the
  address). Same for the security PGP private key — offline, never in git, never on Proton.
- Let the site's existing headers do their job (`deploy/nginx-nightdrop.conf` sets HSTS, CSP,
  `nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options`).
- Consider **DNSSEC** at the registrar and **CAA** records pinning Let's Encrypt.

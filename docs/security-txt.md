# Maintaining `security.txt`

`website/.well-known/security.txt` is a **clearsigned** RFC 9116 file: the served document
is the PGP-signed block, so tampering (or a well-meaning edit) is detectable. Because the
signature covers the exact bytes, **any change invalidates it** — you must re-sign.

- Signing key: `security@nightdrop.app`, fingerprint
  `079B A016 9201 A8AB 11F3  2385 884E ACB8 89D0 2002`.
- Public key: `website/pgp.txt` (served at <https://nightdrop.app/pgp.txt>).
- Private key: **not in this repo** — kept locally (chmod 600). Keep an offline backup.

## When to re-sign

- On each release, or whenever `Expires` nears (keep it **< 1 year** out).
- Any time a field changes (new contact, PGP rotation, domain, policy URL).

## How to re-sign

Edit the field lines, then re-clearsign in place. Using GnuPG with the private key imported
into your keyring:

```sh
# from the repo root; assumes the security key is in your GnuPG keyring
FPR=079BA0169201A8AB11F32385884EACB889D02002

# 1. Edit the *fields* — but the served file is already signed, so start from the
#    plaintext body (strip the PGP wrapper), or keep an unsigned copy to edit.
gpg --decrypt website/.well-known/security.txt > /tmp/security.plain 2>/dev/null   # extract body
$EDITOR /tmp/security.plain                                                        # bump Expires, etc.

# 2. Re-clearsign the edited body back over the served file.
gpg --batch --yes --armor --clearsign --digest-algo SHA512 --local-user "$FPR" \
    --output website/.well-known/security.txt /tmp/security.plain

# 3. Verify before committing.
gpg --verify website/.well-known/security.txt
rm -f /tmp/security.plain
```

To avoid importing the key into your main keyring, sign in a throwaway `GNUPGHOME`:

```sh
export GNUPGHOME=$(mktemp -d); chmod 700 "$GNUPGHOME"
gpg --batch --quiet --import /path/to/private-key.asc
# ... run the clearsign as above ...
rm -rf "$GNUPGHOME"; unset GNUPGHOME
```

## If you rotate the key

Regenerate the keypair (the `pgp-keygen` tool), replace `website/pgp.txt` with the new
armored public key, update the fingerprint in `SECURITY.md` and the comment inside
`security.txt`, then re-sign with the new key.

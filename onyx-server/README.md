# Onyx sync server

Zero-knowledge, self-hosted sync + backup relay. It stores only encrypted
blobs and an oplog; it cannot read a single note or filename.

## Run it

### Docker Compose (recommended — automatic HTTPS)

```sh
export ONYX_DOMAIN=notes.yourdomain.com   # DNS A record → this host
docker compose up -d
```

Caddy fetches a Let's Encrypt certificate automatically; point your Onyx
clients at `https://notes.yourdomain.com`.

### Single binary

```sh
onyx-server --data-dir ./data --listen 0.0.0.0:7677
```

Put it behind a TLS-terminating reverse proxy in production.

## What it stores

`/data/onyx-server.db` (SQLite): device public keys, session tokens
(hashed), vault membership, the encrypted op log, encrypted attachment
blobs, and enrollment relay entries. Every payload is ciphertext the
server has no key for. Back up `/data` with any tool — it's inert without
client keys.

## Multi-user

Users are flat; sharing a vault is `POST /v1/vaults` from each member's
device (they exchange the sync code or pair via the SAS flow). No accounts
to provision.

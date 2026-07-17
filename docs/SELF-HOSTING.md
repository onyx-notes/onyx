# Self-hosting `onyx-server`

`onyx-server` is the optional live-sync backend. You only need it if you want
real-time sync across devices; the app works fully offline without it, and
encrypted backups to S3 / WebDAV / a folder don't require it either.

The server is **zero-knowledge**: it stores only encrypted operations and
content-addressed encrypted blobs. It never sees a note, a filename, or a key.
That means running it is low-stakes — a compromised server leaks ciphertext,
not your notes.

## Quick start (binary)

```sh
onyx-server --data-dir /var/lib/onyx --listen 0.0.0.0:7677
```

- `--data-dir` (default `./data`) — where the encrypted SQLite database lives.
- `--listen` (default `0.0.0.0:7677`) — bind address.

Logging is controlled by `RUST_LOG` (default `info`).

## Docker

```yaml
# docker-compose.yml
services:
  onyx-server:
    image: ghcr.io/onyx-notes/onyx-server:latest
    command: ["--data-dir", "/data", "--listen", "0.0.0.0:7677"]
    volumes:
      - onyx-data:/data
    restart: unless-stopped
    # Put a TLS-terminating reverse proxy in front (see below).
    expose:
      - "7677"

volumes:
  onyx-data:
```

## TLS

The server speaks plain HTTP; **TLS is the reverse proxy's job**. Terminate
TLS at Caddy, nginx, or Traefik and proxy to `127.0.0.1:7677`. A Caddyfile is
two lines:

```
onyx.example.com {
    reverse_proxy 127.0.0.1:7677
}
```

The client connects over `wss://`/`https://` to the proxy. (Built-in rustls
for single-container setups is on the roadmap.)

## Pairing a vault to the server

In the app: **Settings → Sync**, point it at `https://onyx.example.com`, and
either create a sync code or enroll a new device by comparing the short
authentication string (SAS) shown on both screens. For an encrypted vault the
sync identity is derived from the vault key, so nothing secret is written to
disk to enable sync.

## Backups

Because the data directory is entirely ciphertext, backing the server up is
just backing up `--data-dir` (or the `onyx-data` volume). There is nothing
sensitive to protect beyond availability — the encryption keys never leave
your devices. Snapshot the volume on whatever schedule you like.

Note that the server oplog is **self-bounding**: clients periodically upload
compacted checkpoints and the server prunes superseded operations, so the
database does not grow without limit under normal use.

## Sizing

The server is a single-writer SQLite process behind a mutex — deliberately
modest, and the right amount of database for a personal or family deployment.
It has no CPU-heavy work (no search, no merge, no rendering — all of that
happens on clients), so it runs comfortably on the smallest VPS.

## Health check

`GET /v1/health` returns `ok`. Wire it to your uptime monitor.

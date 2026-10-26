# Deployment and backups

Running a pyn server you keep: one node with Docker Compose, TLS in front, upgrades, several copies, and backups with a
tested restore. Provisional: the first release has not been published yet.

## What runs

- `pyn-server`: the HTTP API, one static-config process. State lives in two places only: PostgreSQL (accounts, history,
  locks, sessions, policy) and the object store directory `PYN_DATA_DIR` (file content).
- PostgreSQL 16.
- The `pyn` CLI is a client; install it from the release archives ([releasing](releasing.md)).

The server image is `ghcr.io/avalon-initiative/pyn-core`, tagged with the release (`v1.0.0`; there is no `latest`).
The [`Dockerfile`](../Dockerfile) builds it with `--locked` from pinned base images (tag and digest); it runs as the
non-root user `pyn` (uid 10001) and works with a read-only root filesystem: it writes only to `/data` (a volume) and
`/tmp`. It has a `HEALTHCHECK` on `GET /healthz`. `make docker-build` builds `pyn-core:local` from a checkout for anyone
with Docker; a deployment normally pulls a release instead.

## Single node with Compose

[`docker-compose.yml`](../docker-compose.yml) runs the server, PostgreSQL 16 and two named volumes: `pyn-db` (database)
and `pyn-objects` (`/data`, the object store).

```bash
cp .env.example .env
# fill in the two secrets
sed -i "s/^POSTGRES_PASSWORD=.*/POSTGRES_PASSWORD=$(openssl rand -hex 24)/" .env
sed -i "s/^PYN_SECRET_KEY=.*/PYN_SECRET_KEY=$(openssl rand -hex 32)/" .env
# set PYN_PUBLIC_URL in .env to the address people will use
# set PYN_IMAGE in .env to a release, for example ghcr.io/avalon-initiative/pyn-core:v1.0.0
docker compose pull server
docker compose up --detach --wait
```

Without `PYN_IMAGE` the Compose file builds the image from the checkout (`docker compose up --build --detach --wait`).

`.env` holds secrets and is git-ignored; `.env.example` holds placeholders only. Every variable in
[configuration](configuration.md) can be added to `.env`. Keep `PYN_SECRET_KEY` with the backups: webhook secrets in the
database cannot be decrypted without it.

### First run

A new server serves only setup and `/healthz` until the first administrator exists ([first-run
setup](concepts/access.md#first-run-setup)). The setup token is printed once on standard error at first start, or set
`PYN_SETUP_TOKEN` (at least 16 characters, for example `openssl rand -hex 16`) in `.env` beforehand.

```bash
docker compose logs server | grep -A3 "one-time setup token"     # when PYN_SETUP_TOKEN is not set
curl -fsS http://127.0.0.1:7878/healthz
PYN_SETUP_TOKEN="$TOKEN" pyn --server http://127.0.0.1:7878 setup admin --public-url https://pyn.example.com --registration invite
```

Pass the token in `PYN_SETUP_TOKEN` or on standard input (`--setup-token -`), not as a plain argument. Remove `PYN_SETUP_TOKEN` from `.env` after setup; the server
warns while it is still set.

### Production settings

| Setting | Why |
| --- | --- |
| `PYN_PUBLIC_URL=https://...` | Marks session cookies `Secure` and is used in links; setup records the address too |
| `PYN_SECRET_KEY` | Encrypts webhook secrets at rest; compose refuses to start without it |
| `PYN_REGISTRATION` | `invite` (default) or `closed` for a private server; `open` needs the [sign-up protections](concepts/access.md#protecting-open-registration) |
| `PYN_TRUST_FORWARDED_FOR=true` | Only behind a proxy that sets `X-Forwarded-For`, so rate limits see client addresses |
| `PYN_ALLOWED_ORIGINS` | Only for a web app served from another origin |
| never `PYN_DEV_AUTH` | It accepts any user header with every permission |

Logs go to standard error as plain text; read them with `docker compose logs`. The port is
published on loopback by default (`PYN_PUBLISH` in `.env`), so only a reverse proxy on the host reaches it.

### The web app

The web app is its own image, `ghcr.io/avalon-initiative/pyn-web:<tag>`, released with the same tag scheme as `pyn-core`. It serves the
app and proxies `/v1` and `/healthz` to the server, so people use one address. It is optional in the Compose file (profile `web`):

```
# in .env
PYN_WEB_IMAGE=ghcr.io/avalon-initiative/pyn-web:v1.0.0
PYN_PUBLIC_URL=https://pyn.example.com       # the address people use, in front of the web container
PYN_TRUST_FORWARDED_FOR=true                 # the web container sets X-Forwarded-For
docker compose --profile web up -d --wait
```

The web container listens on `127.0.0.1:8080` by default (`PYN_WEB_PUBLISH` changes it); put the TLS proxy in front of it, not in front of
the server. `PYN_API_URL` (set to `http://server:7878` in the Compose file) is the only setting the image reads; it must have no path.
Upgrade both images together to the same tag.

### Reverse proxy and TLS

The server speaks plain HTTP; terminate TLS in front of it with any proxy (Caddy, nginx, Traefik, a cloud load
balancer). Requirements:

- Forward to `127.0.0.1:7878` and send `X-Forwarded-Proto: https` and `X-Forwarded-For`.
- Allow large request bodies and long uploads: no body size limit (or one above your largest file), and no response or
  request buffering for object routes.
- Redirect HTTP to HTTPS and send HSTS from the proxy.

A minimal Caddy site, which handles certificates and the forwarded headers itself:

```
pyn.example.com {
	reverse_proxy 127.0.0.1:7878
}
```

### Health and monitoring

`GET /healthz` answers 200 once the process is up, including before setup. Docker uses it for the container health
(`docker compose ps`); point an uptime monitor at the public URL. Watch free space on the `pyn-objects` and `pyn-db`
volumes: content is only added, and [limits](concepts/limits.md) are off by default on a self-hosted server.

## Upgrading

Migrations run at server start and take a database lock, so an upgrade is a restart on the new image.

1. [Back up](#backups).
2. Set `PYN_IMAGE` in `.env` to the new tag, then `docker compose pull server && docker compose up --detach --wait`.
3. Check `docker compose logs server` for the migration lines and `curl -fsS http://127.0.0.1:7878/healthz`.

Migrations only move forward; to go back, restore the backup taken in step 1 onto the older image.

## Several server copies

Setting `PYN_INSTANCES` above 1 declares that several server processes share one database; the server then refuses to
start without `PYN_DATABASE_URL` and `PYN_DATA_DIR`. What is and is not safe is listed in
[scale-out](concepts/scale-out.md). In practice:

- All copies use the same PostgreSQL database and the same `PYN_DATA_DIR` on shared storage (a network file system that
  supports atomic rename and hard links), the same `PYN_SECRET_KEY`, and the same `PYN_SETUP_TOKEN` until set up.
- Put a load balancer in front; sessions and CSRF live in the database, so any copy can answer any request.
- Keep clocks in sync (NTP). Set `PYN_WEBHOOK_WORKER=false` on copies that should not deliver webhooks, but leave at least
  one delivering.
- Rate-limits on uploads and expensive requests are per process.

The Compose file is single node; for several copies run the same image as separate services or hosts with the shared
volume mounted at `/data`.

## Backups

State is the PostgreSQL database plus the object store directory, and `PYN_SECRET_KEY`. Back up all three; the database
alone is not enough, since file content lives only in the object store.

[`scripts/backup.sh`](../scripts/backup.sh) writes `pyn-<UTC timestamp>/` containing `db.dump` (`pg_dump --format custom`),
`objects.tar` and `SHA256SUMS`, then removes backups older than `RETENTION_DAYS` (default 14).

```bash
scripts/backup.sh /srv/pyn-backups           # or BACKUP_DIR=... RETENTION_DAYS=30 scripts/backup.sh
```

It works on a running server. The dump is taken first and the objects after: content is uploaded before the revision that
names it, and objects are never changed once written, so the object copy always holds everything the dump refers to (it
may hold a few unreferenced uploads, which the server sweeps later). Taking them in the other order could leave a revision
without its content.

Schedule it (for example a daily cron entry or systemd timer), copy the backup directory off the host, and keep
`PYN_SECRET_KEY` in a password manager or secret store, never next to the dump. A backup that has not been restored is not
a backup: restore into a throwaway deployment now and then.

### Restore

[`scripts/restore.sh`](../scripts/restore.sh) replaces the database and objects of the Compose deployment with a backup:

```bash
scripts/restore.sh --yes /srv/pyn-backups/pyn-20260101T040000Z
```

It verifies `SHA256SUMS`, stops the server, recreates the database from `db.dump`, empties and refills `/data` from
`objects.tar`, and starts the server. On a new host, first create `.env` with the original `PYN_SECRET_KEY` and a new
`POSTGRES_PASSWORD`, run `docker compose up --detach --wait db`, then run the script. Afterwards run `pyn verify` against
a repository to check history integrity ([integrity](concepts/integrity.md)).

The scripts target the Compose deployment; for another setup the same two parts apply: `pg_dump --format custom` of the
database and an archive of `PYN_DATA_DIR`, dump first.

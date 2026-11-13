# Deployment and backups

New here? [Getting started](getting-started.md) leads with an installer script (`scripts/install.sh`) and keeps the step-by-step version, with image-only Compose files in
[`deploy/`](../deploy/docker-compose.yml). This page covers running a server you keep: one node with Docker Compose, TLS in front, upgrades, several copies, and backups with a
tested restore. Provisional: written against the first pre-release, `v0.1.0-alpha`; the image-only Compose file in `deploy/` has not been run against a release yet.

## What runs

- `pyn-server`: the HTTP API, one static-config process. State lives in two places only: PostgreSQL (accounts, history,
  locks, sessions, policy) and the object store directory `PYN_DATA_DIR` (file content).
- PostgreSQL 16.
- The `pyn` CLI is a client; install it from the release archives ([releasing](releasing.md)).

The server image is `ghcr.io/avalon-initiative/pyn-core`, tagged with the plain version only (`v1.0.0`, also for a release titled `v1.0.0-first-stable`). There is no `latest`.
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

### Deployment shapes

`deploy/` holds one self-contained Compose file per shape, each with its own env example, so there are no profiles or
extra flags: download the pair, save them as `docker-compose.yml` and `.env`.

| Shape | Compose file | Env example | Notes |
| --- | --- | --- | --- |
| Server and web app | `docker-compose.yml` | `.env.example` | The default; the web app proxies the API |
| Server only | `docker-compose.server.yml` | `.env.server.example` | API published on `PYN_SERVER_PUBLISH` (default `127.0.0.1:7878`); no web app |
| Web app only | `docker-compose.web.yml` | `.env.web.example` | Needs `PYN_API_URL`; no database or secrets |
| Client only | none | none | Install the `pyn` command |

For web-app-only, the server behind `PYN_API_URL` must list the web app's origin in `PYN_ALLOWED_ORIGINS` (see below).
Healthchecks: the database and server files wait on PostgreSQL and the server image's check; `docker compose up --wait`
on the web-only file waits on the web image's own check, if it has one. These files have not been run against a release yet.

### First run

The easiest way is the browser: `docker compose up -d`, read the setup token from the logs, open the address, enter the token
and step through the wizard. The line printed under the token says where: with `PYN_PUBLIC_URL` set it reads
`finish setup in the browser at <PYN_PUBLIC_URL>, or run: pyn setup <username>`, and without it
`finish setup with the CLI: pyn setup <username>`.
[Getting started](getting-started.md#5-set-up-the-server-first-administrator) walks through it. The `pyn` command works too,
from a computer that has it (it is not inside the server container). A new server serves only setup and `/healthz` until the first administrator exists ([first-run
setup](concepts/access.md#first-run-setup)). The setup token is printed once on standard error at first start, or set
`PYN_SETUP_TOKEN` (at least 16 characters, for example `openssl rand -hex 16`) in `.env` beforehand.

```bash
docker compose logs server | grep -A4 "one-time setup token"     # when PYN_SETUP_TOKEN is not set
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
| `PYN_ALLOWED_ORIGINS` | Required for a web app served from another origin, such as a web-only deployment pointing here (its address, no path) |
| never `PYN_DEV_AUTH` | It accepts any user header with every permission |

Logs go to standard error as plain text; read them with `docker compose logs`. The port is
published on loopback by default (`PYN_PUBLISH` in `.env`), so only a reverse proxy on the host reaches it.

### The web app

The web app is its own image, `ghcr.io/avalon-initiative/pyn-web:<tag>`, released with the same tag scheme as `pyn-core`. It serves the
app and proxies `/v1` and `/healthz` to the server, so people use one address. It is optional in the Compose file (profile `web`); without `PYN_WEB_IMAGE` the profile cannot pull an image, and a server-only deployment never reads it:

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

## Native install (systemd)

`scripts/install.sh --method native --shape server` (see [Getting started](getting-started.md#the-installer-script)) runs
the release `pyn-server` binary without Docker. To do it by hand, on a Linux host with systemd and PostgreSQL 16:

1. Download `pyn-server-<tag>-linux-x86_64.tar.gz` (or `linux-arm64`) and `SHA256SUMS` from the release, run
   `sha256sum --check --ignore-missing SHA256SUMS`, unpack it and `install -m 0755 pyn-server /usr/local/bin/`.
2. Create the user and state directory: `useradd --system --user-group --home-dir /var/lib/pyn --no-create-home --shell /usr/sbin/nologin pyn`,
   `install -d -m 0750 -o pyn -g pyn /var/lib/pyn`.
3. Create a role and database in PostgreSQL (`create role pyn login password '...'; create database pyn owner pyn;`).
4. Write `/etc/pyn/pyn.env`, mode `600`, with `PYN_ADDR` (`0.0.0.0:7878`, or `127.0.0.1:7878` behind a proxy),
   `PYN_DATABASE_URL`, `PYN_SECRET_KEY` (`openssl rand -hex 32`), `PYN_PUBLIC_URL` and `PYN_DATA_DIR=/var/lib/pyn`; add
   anything else from [configuration](configuration.md), such as `PYN_TRUST_FORWARDED_FOR=true` behind a proxy.
5. Install the unit below as `/etc/systemd/system/pyn.service`, then `systemctl daemon-reload && systemctl enable --now pyn`.

```ini
[Unit]
Description=pyn server
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
User=pyn
Group=pyn
EnvironmentFile=/etc/pyn/pyn.env
ExecStart=/usr/local/bin/pyn-server
Restart=on-failure
RestartSec=2
StateDirectory=pyn
StateDirectoryMode=0750
UMask=0077
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
CapabilityBoundingSet=
AmbientCapabilities=
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=true
RestrictSUIDSGID=true
LockPersonality=true
SystemCallArchitectures=native

[Install]
WantedBy=multi-user.target
```

This mirrors the Compose service: a non-root user, no capabilities, a read-only system and one writable directory. The
setup token is in the journal at first start (`journalctl -u pyn | grep -A3 "one-time setup token"`) and
`curl -fsS http://127.0.0.1:7878/healthz` checks the server. To upgrade, back up, replace the binary with the new
release's (checksum verified), and `systemctl restart pyn`; the installer does exactly that when re-run. The installer can also point the unit at an existing env file (`--env-file`), referenced in place through `EnvironmentFile=`; keep it mode `600`. The web app is
Docker-only: its proxy configuration ships in the image. Provisional: not yet run against a release on a real host.

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
alone is not enough, since file content lives only in the object store. Keep the `.env` file (or `/etc/pyn/pyn.env` on a
native install) too, in a secret store: it holds the secret key and the database password, and `scripts/backup.sh` never
copies it, on purpose, so it does not sit next to the dump. See [Recovery](#recovery) for what each loss costs.

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
`PYN_SECRET_KEY` and the `.env` file in a password manager or secret store, never next to the dump. A backup that has not been restored is not
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

## Recovery

Recovery needs control of the host, the same trust level as the one-time setup token. No HTTP route does any of it, so
there is no remote back door; whoever can run commands on the server host (or in its container) can recover it.

### Lost administrator

The server never removes or disables its last active administrator, so if that person leaves or loses their credentials,
nobody can administer it through the API. On the host, run `recover-admin` with the same database settings the server
uses (`PYN_DATABASE_URL`; with Compose the container already has them):

```bash
docker compose exec server pyn-server recover-admin alice
echo 'a long new password' | docker compose exec -T server pyn-server recover-admin alice --password-stdin
```

Natively, run `pyn-server recover-admin alice` as the `pyn` user with the variables from `/etc/pyn/pyn.env` loaded. The
container has `pyn-server` but no `pyn` command; sign in afterwards from any computer that has it, or on the web.

The command creates `alice` as an administrator, or, if the account exists, enables it, completes its sign-up, grants the
administrator flag and sets a new password. Without `--password-stdin` it generates a strong password and prints it once on
standard output (messages go to standard error). The account's sessions and tokens end; its SSH keys and linked external
identities are left as they are. Other accounts are not touched. It refuses an organization name, the in-memory store (no
`PYN_DATABASE_URL`), and a server that has not run [first-run setup](concepts/access.md#first-run-setup) yet.

Each recovery is written to the server audit log (`GET /v1/admin/audit`) as `admin_granted`, plus `account_enabled` when
it lifted a disable, with the actor `@host-recovery`, and to the server log as a `host recovery` warning.

The server warns once at start when exactly one active administrator exists. Add a second one so a single lost account does
not need this.

### Lost secret key

`PYN_SECRET_KEY` encrypts one thing: the signing secrets of [webhooks](concepts/webhooks.md#encryption-at-rest), at rest in
the database. Losing it loses only those secrets; accounts, passwords, tokens, history and files do not depend on it. Without
the key the server still starts, but deliveries to webhooks whose secrets were encrypted fail until they are replaced. To
recover: set a new `PYN_SECRET_KEY` (`openssl rand -hex 32`), then for each webhook run `pyn admin webhook rotate <id>`
(or use the web app), which stores a new secret under the new key; hand it to the receiver and retire the unreadable one
(`pyn admin webhook retire`). Deleting and re-creating the webhook works too.

To change the key while you still have it, stop the server and run, with the current key in `PYN_SECRET_KEY` and the new
one in `PYN_NEW_SECRET_KEY`:

```bash
docker compose stop server
new=$(openssl rand -hex 32)
docker compose run --rm -e PYN_NEW_SECRET_KEY="$new" server rotate-secret-key
sed -i "s/^PYN_SECRET_KEY=.*/PYN_SECRET_KEY=$new/" .env && docker compose up --detach --wait
```

Store the new key in your secret store as well. The command
re-encrypts every stored secret, retired ones and plaintext ones included, in one database transaction: either all are
rewritten or, if any cannot be decrypted with the current key, none, and it says so. Run it once; afterwards the old key no
longer opens anything.

### Lost database password

The password is in the host's `.env` (`POSTGRES_PASSWORD`, and inside `PYN_DATABASE_URL` for a native install) and in the
running container's environment: `docker compose exec server printenv PYN_DATABASE_URL`. If you only need to set a new one,
`docker compose exec db psql --username pyn -c "ALTER USER pyn PASSWORD '...'"` (the Compose database trusts local
connections inside its container) and update `.env`, then restart the server.

### Lost `.env`

Rebuildable: `PYN_PUBLIC_URL`, `PYN_IMAGE` and every tuning setting (see [configuration](configuration.md)), and the database
password, which can be reset as above while the database volume survives. Not rebuildable: `PYN_SECRET_KEY`. Recover it from
your secret store; if it is gone, follow [Lost secret key](#lost-secret-key). Accounts and history live in the database and
the object store, so a lost `.env` alone loses neither.

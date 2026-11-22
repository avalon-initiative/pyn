# Deployment and backups

New here? [Getting started](getting-started.md) leads with an installer script (`scripts/install.sh`) and keeps the step-by-step version, with image-only Compose files in
[`deploy/`](../deploy/docker-compose.yml). This page covers running a server you keep: one node with Docker Compose, TLS in front, upgrades, several copies, and backups with a
tested restore. Provisional: the default image-only Compose file in `deploy/` has been run against the releases `v0.1.0-alpha` to `v0.1.3-alpha` (the smoke test starts it from the published images and completes setup); the server-only and web-only files have not been run against a release yet.

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
| `PYN_PUBLIC_URL=https://...` | Marks session cookies `Secure` and is used in links; setup records the address too. |
| `PYN_WEBAUTHN_RP_ID` | Optional. [Passkeys](concepts/access.md#passkeys-and-security-keys) use `PYN_PUBLIC_URL`'s host as the WebAuthn relying-party id and its origin as the allowed origin, so that address must be a host name over `https://` (an IP address, or `http://` other than `localhost`, leaves authenticator apps as the only second factor). Set this to a parent domain of the host to share passkeys across subdomains. Choose it before people register passkeys: changing the host or the id makes them unusable |
| `PYN_SECRET_KEY` | Encrypts webhook secrets at rest; compose refuses to start without it |
| `PYN_ACCESS` | `private` (default) refuses every signed-out request except sign-in, sign-up and setup; `public` lets signed-out visitors read public repositories and profiles. Used by first-run setup and as the default until it; after setup the stored value wins and an administrator changes it at runtime (see [server access mode](concepts/access.md#server-access-mode)) |
| `PYN_REGISTRATION` | `invite` (default) or `closed` for a private server; `open` needs the [sign-up protections](concepts/access.md#protecting-open-registration) |
| `PYN_TRUST_FORWARDED_FOR=true` | Only behind a proxy that sets `X-Forwarded-For`, so rate limits see client addresses. The Compose files set it. The web container's nginx appends its own hop to the header, so count the proxies with `PYN_TRUSTED_PROXIES`: 1 plus the trusted appending proxies in front of the web container (1 with nothing in front, 2 behind the installer's Caddy): see [the layouts](#public-address-and-client-addresses-per-layout) |
| `PYN_ALLOWED_ORIGINS` | Required for a web app served from another origin, such as a web-only deployment pointing here (its address, no path) |
| `PYN_DISABLED_ACCOUNT_RETENTION_DAYS` | Disabled accounts are [deleted](concepts/access.md#deleting-an-account) after 90 days by default (`0` keeps them forever); the server runs the task itself (hourly), so nothing is scheduled outside it |
| never `PYN_DEV_AUTH` | It accepts any user header with every permission |

Logs go to standard error as plain text; read them with `docker compose logs`. The API port
7878 is published on loopback only; browsers reach the web container, with a proxy in front for https ([HTTPS](#https)).

### Checking the configuration

Before `docker compose up`, or after editing `.env`, run `docker compose run --rm --no-deps server pyn-server --check-config`. It starts nothing and lists every wrong or inconsistent `PYN_*` setting, and any `PYN_` variable the server does not read. See [checking a configuration](configuration.md#checking-a-configuration).

### Email

pyn sends email only for address verification on an open-registration server. It is optional, and nothing is sent unless you configure it:

- **none** (default): no mailer. Open sign-up then needs no email address and does no verification, so a second person can create an account at once. Pair open registration with `PYN_REQUIRE_APPROVAL=true` if you want a gate.
- **log** (`PYN_MAILER=log`): for trying things out. Each message, with its verification link, is written to the server log (`docker compose logs server | grep -A8 PYN-MAILER-LOG`); nothing is delivered. The links are secrets, so the server warns at start and refuses it on a non-loopback `PYN_ADDR`. The container listens on `0.0.0.0`, so under Compose it also needs `PYN_MAILER_LOG_ALLOW_NON_LOOPBACK=true`, which is safe only while the port stays published on loopback and nobody else reads the log.
- **smtp** (`PYN_MAILER=smtp`): real delivery through any SMTP server you can use, such as your provider's. Set `PYN_SMTP_HOST`, `PYN_SMTP_FROM`, usually `PYN_SMTP_USER` and `PYN_SMTP_PASSWORD`, and optionally `PYN_SMTP_PORT` (587) and `PYN_SMTP_TLS` (`starttls`, `tls` or `none`). Verification turns on by itself; the settings are in [configuration](configuration.md) and `deploy/.env.example`.

A message that cannot be sent never blocks the sign-up that asked for it; the failure is logged (`email delivery failed`, the recipient's domain and the reason, never the password) and the person asks an administrator.

### The web app

The web app is its own image, `ghcr.io/avalon-initiative/pyn-web:<tag>`, versioned and released on its own, separately from `pyn-core` (its tags are its own). It serves the
app and proxies `/v1` and `/healthz` to the server, so people use one address. It is optional in the Compose file (profile `web`); without `PYN_WEB_IMAGE` the profile cannot pull an image, and a server-only deployment never reads it:

```
# in .env
PYN_WEB_IMAGE=ghcr.io/avalon-initiative/pyn-web:v1.0.0
PYN_PUBLIC_URL=https://pyn.example.com       # the address people use, in front of the web container
PYN_TRUST_FORWARDED_FOR=true                 # the web container appends to X-Forwarded-For; PYN_TRUSTED_PROXIES counts it
docker compose --profile web up -d --wait
```

The web container listens for plain HTTP on `127.0.0.1:8080` by default (`PYN_WEB_PUBLISH` changes it); put the TLS proxy in front of it ([HTTPS](#https)), not in front of
the server. `PYN_API_URL` (set to `http://server:7878` in the Compose file) is the only setting the image reads; it must have no path.
The two images are set separately (`PYN_IMAGE` and `PYN_WEB_IMAGE` here; `PYN_SERVER_VERSION` and `PYN_WEB_VERSION` in the
released files under `deploy/`), their tags need not match, and either can be upgraded alone: change its line, then
`docker compose pull` and `docker compose up -d`.

### Compatibility

The server and the web app are released independently, and no release requires a specific version of the other.

- The HTTP API is additive within `/v1`: fields and routes are added, never removed or changed. A newer server never breaks an older web app.
- A web app that is newer than its server may use endpoints the server lacks. Each web release states the oldest server it needs
  (`min_server`) in its release notes, and the app shows a notice when the server it talks to is older than that.
- `GET /v1/capabilities` reports the server's release in `version` (readable signed out, in both access modes), which is what the
  web app compares against.

### HTTPS

The web container serves the app and forwards `/v1`, `/healthz` and `/readyz` to the server over plain HTTP on 8080, and holds no certificate. HTTPS comes from a proxy in front of it: browsers and the `pyn` command then use one https address. Put the proxy in front of the web container, not the server's API port 7878 (that would skip the web app), and keep 7878 on loopback or a private network, where it carries no TLS.

Without a proxy there is no https, and so no [passkeys](#why-passkeys-need-a-host-name), except on `http://localhost:8080`, which browsers treat as secure. Until https is in place, the CLI warns about the clear-text address: at `pyn login` and when `pyn config set server`
stores an `http://` address for a host that is not loopback. The warning is shown once per address (stored in the user's
configuration directory, next to the saved sign-ins) and again after a new login or a changed address; later commands
print nothing extra, and `--json` output is unaffected.

#### Choosing a recipe

In this order of preference:

1. **Hosted:** nothing to do. A hosted pyn uses a publicly trusted certificate.
2. **Stock Caddy by hand**, for a public host name whose DNS points at this host and whose ports 80 and 443 are reachable from the internet. Caddy gets and renews a Let's Encrypt certificate, redirects http to https and sets the forwarded headers itself. Run it on the host (or in your own container) with a three-line Caddyfile and `PYN_WEB_PUBLISH=127.0.0.1:8080` in `.env`:

   ```
   pyn.example.com {
   	reverse_proxy 127.0.0.1:8080
   }
   ```

   Caddy has no request body limit and no timeouts that cut a push, an upload or the 25 second lock streams by default. Add `flush_interval -1` inside a `reverse_proxy` block to stream responses as they arrive. Set `PYN_PUBLIC_URL=https://pyn.example.com` and `PYN_TRUSTED_PROXIES=2` ([why](#public-address-and-client-addresses-per-layout)).
3. **The installer's Caddy option**: the same Caddy as a `caddy` container on the official `caddy` image, next to the stack, from the same `.env` ([below](#the-installers-caddy-option)). It also covers an address or a private name with Caddy's own local authority.
4. **A managed balancer that terminates TLS** (a cloud load balancer or an Ingress with a managed certificate) in front of the web container on 8080, as in the [load balancer guide](load-balancing.md#tls). Nothing to renew by hand. Its idle timeout must stay above 30 seconds and it must not cap request bodies ([timeouts](load-balancing.md#timeouts)).
5. **Traefik or nginx with your own certificate**, for an authority of your own or a certificate you already manage. Forward to `http://web:8080` (or `127.0.0.1:8080`), send `X-Forwarded-Proto: https` and the client address in `X-Forwarded-For`, allow any body size, do not buffer, and read for at least 300 seconds. An nginx server block, with the certificate lines yours:

   ```
   server {
       listen 443 ssl;
       server_name pyn.example.com;
       ssl_certificate     /etc/ssl/pyn.example.com.pem;
       ssl_certificate_key /etc/ssl/pyn.example.com.key;
       client_max_body_size 0;
       location / {
           proxy_pass http://127.0.0.1:8080;
           proxy_http_version 1.1;
           proxy_set_header Host $host;
           proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
           proxy_set_header X-Forwarded-Proto $scheme;
           proxy_request_buffering off;
           proxy_buffering off;
           proxy_read_timeout 300s;
           proxy_send_timeout 300s;
       }
   }
   ```

   Traefik sets the forwarded headers itself; point a router with a TLS certificate at the service `http://web:8080`.
6. **A Cloudflare tunnel**, only for people who cannot open ports, never a default and never required. `cloudflared` forwards to `http://web:8080`. The limits are real: Cloudflare caps request bodies (100 MB on the lower plans), so large pushes and uploads fail, and it carries HTTP only, so the planned SSH transport ([#68](https://github.com/avalon-initiative/pyn/issues/68)) does not pass through it without extra client software on every machine. It is not for production traffic.
7. **Advanced: a DNS challenge**, for a name on a private network in a zone you control (nothing exposed to the internet, public certificates, nothing to install on devices). The official image has no DNS provider modules, so use a Caddy build that has the one you need through `PYN_CADDY_IMAGE`, and edit the Caddyfile by hand. Provisional: the recipe below is not tested in this repository.

   ```dockerfile
   FROM caddy:2.11-builder AS build
   RUN xcaddy build --with github.com/caddy-dns/cloudflare
   FROM caddy:2.11
   COPY --from=build /usr/bin/caddy /usr/bin/caddy
   ```

   Build it (`docker build -t pyn-caddy-cloudflare .`), set `PYN_CADDY_IMAGE=pyn-caddy-cloudflare` in `.env`, and replace the site block of `./Caddyfile` with the token read from the environment (the installer keeps a Caddyfile it did not write for the same name):

   ```
   pyn.example.com {
   	tls {
   		dns cloudflare {env.CF_API_TOKEN}
   	}
   	reverse_proxy web:8080 {
   		flush_interval -1
   	}
   }
   ```

   Give the `caddy` service the token in a `docker-compose.override.yml` next to the Compose file (`env_file: ./cloudflare.env`, a file holding `CF_API_TOKEN=...`, mode 600, never committed). Create the token in the Cloudflare dashboard (My Profile, API Tokens) with **Zone: Zone: Read** and **Zone: DNS: Edit**, limited to the one zone; it can change that zone's DNS, so revoke it if the host is lost. Add one DNS record for the name (an A record to this host's private address, DNS only).

#### The installer's Caddy option

`scripts/install.sh` asks "Put Caddy in front for HTTPS?" ([Getting started](getting-started.md)); `--caddy --site NAME` answers it for an unattended run. It asks only for the domain name or address people use, then:

- picks the certificate kind from the name and says why: a public name gets automatic certificates (Let's Encrypt, with a second public authority enabled as a fallback), an IP address or a private name (a single label, or one ending in `.lan`, `.local`, `.home.arpa`, `.internal`, `.test` or `.localhost`) gets Caddy's own local authority (`tls internal`), whose root each device trusts once ([below](#trusting-the-internal-root));
- checks that ports 80 and 443 are free on the host (and for a public name says they must be reachable from the internet), and refuses a Compose file that has no `caddy` service with its persistent `caddy-data` volume;
- writes `./Caddyfile` next to the Compose file, sets `COMPOSE_PROFILES=caddy`, `PYN_SITE`, `PYN_PUBLIC_URL=https://<name>`, `PYN_TRUST_FORWARDED_FOR=true` and `PYN_TRUSTED_PROXIES=2` in `.env`, and starts the stack;
- for the local authority, copies the root out of the container to `./pyn-root.crt` and prints how to trust it.

The `caddy` service is in the Compose file behind the profile `caddy`: `docker compose up -d` without the profile is exactly the plain HTTP stack, and `COMPOSE_PROFILES=caddy` in `.env` is all that switches it on, for the installer and by hand. Its pieces:

| Setting | Meaning |
| --- | --- |
| `PYN_SITE` | The host name or IP address people use, with no scheme or port. Read by the installer when it re-runs; the Caddyfile holds the name |
| `PYN_CADDY_IMAGE` | The image, `caddy:2.11` pinned by digest by default. A build with a DNS provider module goes here ([recipe 7](#choosing-a-recipe)) |
| `PYN_WEB_PUBLISH_HTTP`, `PYN_WEB_PUBLISH_TLS` | Where ports 80 and 443 (and 443/udp, HTTP/3) are published: `ip:port` for each, default all interfaces on 80 and 443 |
| `PYN_WEB_PUBLISH` | The plain HTTP port of the web container, `127.0.0.1:8080` by default; leave it on loopback so https is the only way in |

The Caddyfile it writes (the `tls internal` line only for the local authority):

```
{
	skip_install_trust
	default_sni pyn.example.com
}

pyn.example.com {
	reverse_proxy web:8080 {
		flush_interval -1
	}
}
```

`flush_interval -1` and Caddy's default of no response timeout stream responses and let a push or the 25 second lock streams run as long as they need; Caddy has no body limit or request buffering. `default_sni` makes a client that connects by IP address, and so sends no server name, get the site's certificate; without it Caddy would look for a certificate for the container's own address behind Docker's port forwarding and fail the handshake. The admin endpoint stays on its default `localhost:2019` inside the container, unpublished: it is what `caddy reload` and the container health check use. The installer keeps an existing Caddyfile for the same name, so hand edits survive a re-run; delete the file to have it written again.

The service runs the stock image as its own root user with `cap_drop: [ALL]` plus `cap_add: [NET_BIND_SERVICE]`, no new privileges, and no other capability. The official binary carries `cap_net_bind_service` as a file capability, which exec only accepts while the capability is still in the container's bounding set, so dropping it without adding it back would stop Caddy from starting. Caddy needs a writable `/data` and `/config`, the two named volumes; the Caddyfile is mounted read-only and the installer makes it world-readable (mode 644), since the container has no capability to read files it does not own.

#### Ports and exposure

- With the Caddy option, **80, 443 and 443/udp** are published on all interfaces. Port 80 answers with a redirect to https, and for a public name is also used to prove control of the name. Without it nothing in the Compose files publishes 80 or 443.
- The redirect omits the port, so the real 443 must be the published one. To use other ports, run your own proxy ([recipes](#choosing-a-recipe)).
- A public name needs this host reachable on 80 and 443 **from the internet**, and the name's DNS pointing at it. Where that is not possible, use an address or private name with the local authority, or a DNS challenge ([recipe 7](#choosing-a-recipe)).
- A host that already uses 80 or 443 (another web server, a proxy) cannot also publish them: keep that proxy in front of `127.0.0.1:8080`, or stop the other one.
- Exposed to the network is only what the proxy forwards: the app, `/v1`, `/healthz` and `/readyz`. The server's API port, the database and Caddy's administration endpoint are not published.
- Local authority: anyone who can read the `caddy-data` volume holds the authority's private key, so devices that trust the root would trust a forged certificate. Treat the volume as a secret and trust the root only on devices that need it.

#### Why passkeys need a host name

[Passkeys](concepts/access.md#passkeys-and-security-keys) are tied to a host name by the web standard (WebAuthn): an IP address is not allowed, and the page must be https (or `localhost`). With an IP address people
sign in with a password and an authenticator app. To use passkeys, give pyn a name: a public one, a name in your own zone, or a private name such as `pyn.lan` (point it at the host in your router's DNS or in each device's hosts file) with the local authority.
Choose the name before people register passkeys; changing it makes them unusable.

#### Trusting the internal root

Needed only for the local authority. Fetch the root from the running container (the installer saves it as `pyn-root.crt` next to `.env`):

```bash
docker compose cp caddy:/data/caddy/pki/authorities/local/root.crt ./pyn-root.crt
```

Trust it once on each device that opens pyn; keep it from anyone who should not be trusted. The root persists in the `caddy-data` volume, so a recreated container keeps it.

- **Linux (Debian, Ubuntu):** `sudo cp pyn-root.crt /usr/local/share/ca-certificates/pyn-root.crt && sudo update-ca-certificates`. Fedora and similar: copy to `/etc/pki/ca-trust/source/anchors/` and run `sudo update-ca-trust`. Firefox keeps its own list: Settings, Privacy and Security, Certificates, View Certificates, Authorities, Import, and tick "Trust this CA to identify websites". Chrome and Chromium on Linux use the per-user NSS store: `certutil -d sql:$HOME/.pki/nssdb -A -t "C,," -n pyn-root -i pyn-root.crt`.
- **macOS:** `sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain pyn-root.crt`, or open the file in Keychain Access, add it to System, open it and set "When using this certificate" to Always Trust.
- **Windows:** in an administrator prompt, `certutil -addstore -f ROOT pyn-root.crt`, or double-click the file, Install Certificate, Local Machine, and place it in Trusted Root Certification Authorities. Firefox on Windows can use the system store (`security.enterprise_roots.enabled`) or import it as above.
- **iOS and iPadOS:** send the file to the device (AirDrop, mail or a download), open it and allow the profile, install it in Settings, Profile Downloaded, then turn on full trust in Settings, General, About, Certificate Trust Settings.
- **Android:** Settings, Security (or Security and privacy), Encryption and credentials, Install a certificate, CA certificate, and choose the file. Chrome and the system browser accept a user-installed authority; some apps ignore it.

Provisional: the `pyn` command is expected to read the operating system's trust store, so the steps above should be enough for it too; this has not been verified against an internal root yet.

#### Public address and client addresses per layout

`PYN_PUBLIC_URL` is the address people type, `https://<name>` without a port for the default 443. Sign-in, cookies and passkeys follow it. `PYN_TRUST_FORWARDED_FOR=true` (set
by the Compose files) lets the server's rate limits use the client address from `X-Forwarded-For`, and `PYN_TRUSTED_PROXIES` says how many proxies append to it.

Each proxy that appends puts the address it saw at the end of the header. The web container is one of them: its nginx sets `X-Forwarded-For` to the incoming value plus the address of whoever connected to it
(`$proxy_add_x_forwarded_for`, `docker/nginx/default.conf.template` in `pyn-web`), and keeps an `X-Forwarded-Proto` and `X-Forwarded-Host` it already received. The server takes the entry `PYN_TRUSTED_PROXIES` positions from the right
(`address` in `crates/pyn-server/src/client.rs`), so the value is **1 plus the number of proxies in front of the web container that you trust to append**. Anything further left is client-supplied and never used.

| Layout | `PYN_PUBLIC_URL` | `X-Forwarded-For` at the server | `PYN_TRUSTED_PROXIES` |
| --- | --- | --- | --- |
| Direct: the web container is the edge (plain HTTP, a trial or a trusted network) | `http://<host>:8080` | `client` | `1` (default) |
| The installer's Caddy, or Caddy, Traefik or nginx on the same host or network, forwarding to the web container | `https://<name>` | `client, proxy` | `2` |
| A managed balancer that terminates TLS and puts the client in `X-Forwarded-For`, in front of the web container | The balancer's https address | `client, balancer` | `2` |
| A TCP (layer 4) balancer that keeps the client's source address | Its address | `client` | `1` |
| A CDN in front of a balancer in front of the web container, all appending | The public https address | `client, cdn, balancer` | `3` |
| A Cloudflare tunnel (`cloudflared` forwarding to `http://web:8080`) | `https://<the tunnel's host name>` | `client, cloudflared` | `2`, verified with a request you can see in the server log, since the tunnel's header handling is Cloudflare's |

Check each hop's documentation for whether it appends or replaces the header, and count only the ones that append and that you control: a hop that replaces the header does not add an entry. Too low a value makes every client look like the outer proxy and share its rate limits; too high a value picks an entry the client may have written, or falls back to the peer address when the list is shorter. `PYN_TRUSTED_PROXIES` needs `PYN_TRUST_FORWARDED_FOR=true`. The installer sets `PYN_TRUSTED_PROXIES=2` with the Caddy option. When the web app runs on its own host in front of a server elsewhere, set the value on that server's `.env`.

**Servers reached without the web container.** When a proxy talks to the server directly, each proxy that appends the address it saw adds one entry, and `PYN_TRUSTED_PROXIES` equals the number of proxies you trust to append (the web container is not in the chain, so there is no extra one):

| What reaches the server | `PYN_TRUSTED_PROXIES` | Header the server sees |
| --- | --- | --- |
| One proxy or balancer that appends the client (nginx, Caddy, Traefik, a managed balancer) | `1` (default) | `client` |
| A CDN or TLS proxy in front of a balancer, both appending | `2` | `client, cdn` |
| A CDN in front of a balancer in front of a proxy, all appending | `3` | `client, cdn, balancer` |

When another proxy terminates TLS, add the web app's public address to the server's `PYN_ALLOWED_ORIGINS` only for a web app served from another origin.

#### Certificates must persist, and renew on their own

With the Caddy option the certificates and the local root live in the `caddy-data` volume (`/data` in the container). It must persist: losing it means new certificates, for the local authority a new root every device must trust again, and for a public name a
risk of hitting the public authority's rate limits. Never run `docker compose down -v` on a live deployment (that deletes the database volume too); the installer refuses a Compose file without the volume. With a proxy of your own, keep its certificate storage persistent the same way.

Renewal is automatic ([the server watches the result](#the-server-watches-the-certificate)): Caddy renews when about a third of a certificate's life is left, tries the fallback authority if the first fails, and needs no change for the shorter lifetimes public authorities are moving to. If a certificate is
not renewing, check in this order:

1. `docker compose logs caddy` for `certificate`, `acme` or `error` lines; a failed renewal is logged with the reason.
2. For a public name: ports 80 and 443 reachable from the internet and the name's DNS pointing at this host. For a DNS challenge: the token is valid, not revoked, and limited to the right zone with Zone Read and DNS Edit.
3. The `caddy-data` volume is mounted and writable (`docker compose config` shows it), and the host clock is right.
4. The expiry the browser sees: `echo | openssl s_client -connect pyn.example.com:443 -servername pyn.example.com 2>/dev/null | openssl x509 -noout -enddate`. Point an uptime monitor that checks certificate expiry at the public address too.

The release smoke test (`scripts/smoke-release.sh`) runs the Caddy option with an internal certificate, checks the redirect, the https sign-in through both proxies and the certificate's expiry date, and warns loudly when too little life is left.

#### The server watches the certificate

The certificate is held by the proxy in front (the `caddy` container, or your balancer), so the server reads it from outside. Every `PYN_CERT_CHECK_INTERVAL_SECS` (default 21600, six hours; `0` turns it off; otherwise 60 to 604800)
it connects to the host and port of `PYN_PUBLIC_URL`, completes a TLS handshake and reads the certificate's dates and names, then closes the connection. It sends no credentials and no request. The first check runs
30 seconds after start. Nothing is checked, and the state is `unchecked`, when the public URL is not `https://`, when its host is `localhost` or a loopback address, or when the interval is `0`. Behind a balancer
that holds the certificate the check still reads what the balancer serves, provided the public URL is https and this server can reach that name; a host that cannot reach its own
public address (no hairpin routing) reports `unreachable`, and then the expiry has to be watched elsewhere.

An untrusted certificate is read like any other: the local authority and a private CA are normal here, so the handshake does not fail on trust. Whether the chain verifies for that host against the server's system roots is
reported separately as `trusted`; `false` is expected with the local authority, and a surprise with a public name.

`GET /v1/admin/settings` (administrators) reports a `certificate` object: `state`, `host`, `trusted`, `not_before`, `not_after`, `days_left` (whole days, rounded down, negative once expired), `names`, `issuer`,
`last_checked` and `last_error`. Only public certificate facts and the configured host appear. `/readyz` does not depend on it: a certificate problem is not a reason to take a copy out of rotation.

| State | Meaning |
| --- | --- |
| `ok` | Valid, with more than the warning margin left |
| `expiring` | Less remaining than the smaller of 14 days and a third of the certificate's total lifetime (so Caddy's 12 hour internal certificates, renewed at a third of their life, are not flagged for being short) |
| `expired` | Past `not_after` |
| `unreachable` | The last handshake failed (`last_error` says how: refused, timed out, not TLS); the certificate fields keep the last good read |
| `unchecked` | Not applicable or switched off (above), or the first check has not run yet |

The server logs at warn level while a certificate is `expiring`, at error level when under the smaller of 3 days and a quarter of the margin remain or it has expired, at warn level when a check fails, and once at info
when it recovers, each at most once per check interval. A certificate that is not `trusted` is logged once when that is first seen.

When it is `expiring` or `unreachable`, follow the renewal checklist above: `docker compose logs caddy` for the renewal error, the `caddy-data` volume, ports 80 and 443 reachable from the internet (public name), the DNS token
(DNS challenge), and the host clock.

#### Upgrading an existing install

An install whose `.env` has no `COMPOSE_PROFILES=caddy` is unchanged: the web container stays plain HTTP on 8080, the Compose file starts no `caddy` service and publishes nothing on 80 or 443, and `PYN_PUBLIC_URL` and the 8080 mapping are used as before. To turn https on with the Caddy option,
refresh the Compose file (it adds the `caddy` service and its volumes) and re-run the installer with `--caddy --site NAME` (it writes the Caddyfile, moves `PYN_PUBLIC_URL` and sets the proxy count, and keeps the rest); by hand, write the Caddyfile, add `COMPOSE_PROFILES=caddy`,
`PYN_SITE`, `PYN_PUBLIC_URL=https://<name>`, `PYN_TRUST_FORWARDED_FOR=true` and `PYN_TRUSTED_PROXIES=2` to `.env`, and run `docker compose up -d`. If your own proxy already serves the name, change nothing but `PYN_TRUSTED_PROXIES` ([layouts](#public-address-and-client-addresses-per-layout)). Both the server and the web app address people use change when you move to https: update the CLI's saved address (`pyn config set --global server https://<name>`) and any web app that lists this one in `PYN_ALLOWED_ORIGINS`.

### Health and monitoring

`GET /healthz` answers 200 once the process is up, including before setup. Docker uses it for the container health
(`docker compose ps`); point an uptime monitor at the public URL. It is a liveness check only and never looks at the
database or the object store.

`GET /readyz` is the readiness check: whether this copy should be sent traffic. It needs no credentials, is exempt from rate limits and
answers before setup too. The body is `{"status": ..., "checks": [{"name": ..., "status": ..., "detail": ...}]}` with no secrets or addresses:

| Answer | Meaning |
| --- | --- |
| `200` `status: "ok"` | The metadata store answered a cheap query (a 2 second limit, the answer reused for 3 seconds so a storm of probes costs one query) and the object store is not `degraded` (the probe behind `GET /v1/admin/settings`, about every 30 seconds for a bucket; the memory and filesystem stores have no probe and count as ok). |
| `503` `status: "unavailable"` | A check failed; the one in `checks` with `status: "failing"` names it (`metadata_store` or `object_store`). |
| `503` `status: "draining"` | The server got a stop signal and is shutting down ([below](#stopping-and-restarting)). |

Which to point at what:

- A container health check or a process supervisor that restarts on failure: `/healthz`. A database or bucket outage
  should not restart pyn, because a restart fixes neither.
- A load balancer, Kubernetes `readinessProbe` or other thing that decides where to send requests: `/readyz`, so a copy
  that cannot reach its stores, or is shutting down, gets no traffic. With several copies a shared outage makes every copy
  not ready at once; that is the check working, though a balancer that treats "none ready" as an emergency should be set
  to keep serving or show its own error page.
- A Kubernetes `livenessProbe` and `startupProbe`: `/healthz`.

The balancer's own health check interval decides how soon it stops sending. Without a drain delay the server answers `503` only on connections that are still open when
it starts draining, and then closes its listener, so a new probe gets a refused connection, which balancers also count as a failure. With `PYN_SHUTDOWN_DRAIN_DELAY_SECS` the listener stays open and `/readyz` answers `503 draining` on new connections too, until the delay ends.

### Stopping and restarting

On `SIGTERM` (what `docker stop`, `docker compose stop`, systemd and Kubernetes send) or `SIGINT` (Ctrl-C) the server:

1. marks itself draining, so `/readyz` answers `503 draining` at once;
2. keeps accepting connections for `PYN_SHUTDOWN_DRAIN_DELAY_SECS` (default 0; whole seconds, at most 3600), then stops
   accepting. During the delay every request other than the readiness probe is served in full and counts as in flight,
   so a balancer that has not noticed yet still gets answers. A second signal during the delay skips the rest of it;
3. lets requests already running finish, for up to `PYN_SHUTDOWN_GRACE_SECS` (default 30; whole seconds; `0` means no
   grace: stop at once after step 2). The grace counts from the moment the listener closes, not from the signal, so
   the longest a stop takes is delay plus grace;
4. ends [lock streams](concepts/events.md) (`GET /v1/repos/{owner}/{name}/locks/events`) immediately with the page it has,
   without holding the shutdown for the rest of its wait. `pyn locks --watch` reconnects from its cursor on its own,
   retrying with backoff until a server answers, so a restart is a short pause, not a lost event. This happens in step 1,
   at the start of the delay;
5. stops the background loops (webhook delivery, the expiry and cleanup sweeps, the object store probe and the update
   check) so they start no new pass. A pass already running is not waited on: webhook deliveries are claimed with a
   lease and are picked up again, and the sweeps are idempotent;
6. exits `0`.

If the grace period ends first, or a second signal arrives during it, the server exits immediately with status `1` and logs how many requests were
cut off. Idle keep-alive connections are not requests: when none was running the exit is `0`. A cut-off upload
is simply retried by the client; no partial state is committed.

The container runtime must wait longer than the delay plus the grace period before it kills the process, or the settings mean nothing.
Docker's default `stop_grace_period` is 10 seconds and Kubernetes' default `terminationGracePeriodSeconds` is 30, both
too short for the default grace; set them above `PYN_SHUTDOWN_DRAIN_DELAY_SECS` plus `PYN_SHUTDOWN_GRACE_SECS`:

```yaml
# docker-compose.yml, on the server service (the compose files here set it)
stop_grace_period: 45s
```

```yaml
# Kubernetes pod spec
terminationGracePeriodSeconds: 45
```

systemd waits 90 seconds by default (`TimeoutStopSec`), which already covers the default grace. Keep the grace
at least as long as your longest push or upload you want to let finish.

Behind a load balancer that needs time to notice a copy is leaving, set `PYN_SHUTDOWN_DRAIN_DELAY_SECS` a little above the balancer's
health check interval times its unhealthy threshold (for example 5 seconds times 3 failures is 15, so 20). The copy then fails
its readiness checks while it still answers, the balancer takes it out of rotation, and no request meets a closed listener. The default is `0`, so nothing
changes until you set it, and the `stop_grace_period: 45s` in the Compose files is unchanged (it covers the default 30 second grace). If you set a delay, raise it to above delay plus grace:

```yaml
# delay 20 plus grace 30, with margin
stop_grace_period: 60s
```

```yaml
# Kubernetes pod spec, with PYN_SHUTDOWN_DRAIN_DELAY_SECS=20 and PYN_SHUTDOWN_GRACE_SECS=30
terminationGracePeriodSeconds: 60
```
 Watch free space on the `pyn-objects` and `pyn-db`
volumes (with an S3 bucket, `GET /v1/admin/settings` reports whether the store answers): content is only added, and [limits](concepts/limits.md) are off by default on a self-hosted server.

### Shipping logs

The server writes logs to standard output and does not know where they go. For a collector, set `PYN_LOG_FORMAT=json`
(one object per line; the fields are in [log fields](configuration.md#log-fields)), and optionally `PYN_AUDIT_LOG=true`
to put audit events in the same stream. The access line has the request id, route template, status, latency and the
signed-in account, so "who did what, and which request" is one search. Set `RUST_LOG=info` or finer: the `access` and
`audit` lines are `info`. Requests to `/healthz` (the compose health check calls it every few seconds) are never
logged. To quiet the log further, set `PYN_ACCESS_LOG=false` to drop the access lines, or raise the `RUST_LOG` level
(`warn`) to drop every `info` line; the access log stays on by default.

The compose files under `deploy/` keep Docker's `json-file` driver and rotate it: each container keeps at most 5 files of
10 MB, so the logs of a long-running server cannot fill the disk. Change `max-size` and `max-file` in the `x-logging` block at
the top of the file, or switch the driver to send logs elsewhere.

With Compose, a collector reads the container's output through Docker's logging driver or the file under
`/var/lib/docker/containers`. A [Fluent Bit](https://fluentbit.io/) configuration that sends it to a Splunk HTTP Event
Collector (the token is the collector's, set in `SPLUNK_HEC_TOKEN`):

```
[INPUT]
    Name         tail
    Path         /var/lib/docker/containers/*/*-json.log
    Parser       docker
    Tag          pyn

[FILTER]
    Name         parser
    Match        pyn
    Key_Name     log
    Parser       json
    Reserve_Data On

[OUTPUT]
    Name         splunk
    Match        pyn
    Host         splunk.example.com
    Port         8088
    TLS          On
    Splunk_Token ${SPLUNK_HEC_TOKEN}
    Event_Index  pyn
    Event_Sourcetype _json
```

`docker` and `json` are Fluent Bit's stock parsers; add a `Path` or container-name filter when other containers share
the host. [Vector](https://vector.dev/) reads the same output with a `docker_logs` source, a `remap` step that parses
the `message` as JSON, and a `splunk_hec_logs` sink.

Datadog, Loki, CloudWatch and an OpenTelemetry Collector take the same shape: read the container's standard output,
parse each line as JSON, and forward it with the destination's output.

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
start without `PYN_DATABASE_URL` and shared content storage (a bucket, below, or `PYN_DATA_DIR` on a shared volume). What is and is not safe is listed in
[scale-out](concepts/scale-out.md). In practice:

- All copies use the same PostgreSQL database and the same content storage: one S3 bucket ([below](#several-copies-on-a-bucket)),
  or the same `PYN_DATA_DIR` on shared storage (a network file system that supports atomic rename and hard links). They also
  share the same `PYN_SECRET_KEY` and the same `PYN_SETUP_TOKEN` until set up.
- Put a load balancer in front; sessions and CSRF live in the database, so any copy can answer any request. [Load balancing](load-balancing.md) covers the layouts, health checks, draining, timeouts and client addresses.
- Keep clocks in sync (NTP). Set `PYN_WEBHOOK_WORKER=false` on copies that should not deliver webhooks, but leave at least
  one delivering.
- The caps on concurrent uploads, expensive requests and merges are per process on purpose: each bounds one copy's memory and CPU, so N copies allow N times the cap ([scale-out](concepts/scale-out.md)).

The Compose file is single node; for several copies run the same image as separate services or hosts, with a bucket or
with the shared volume mounted at `/data`.

### Several copies on a bucket

With `PYN_S3_BUCKET` the copies need no shared volume: file content lives in an S3-compatible bucket and metadata in
PostgreSQL. The example is DigitalOcean Spaces; another provider changes only the endpoint, region and keys.

1. Create a Space in a region, for example `nyc3`, with file listing restricted (a private bucket: pyn never serves objects
   publicly), and a key pair (Spaces access key and secret) scoped to that Space if the
   provider allows it.
2. Put the settings in the environment of every copy (the Compose file sets `PYN_DATA_DIR`, so remove that line and the
   `pyn-objects` volume from a copy of it, or run the image from your own service definitions):

   ```
   PYN_INSTANCES=2
   PYN_DATABASE_URL=postgres://...
   PYN_S3_ENDPOINT=https://nyc3.digitaloceanspaces.com
   PYN_S3_REGION=nyc3
   PYN_S3_BUCKET=my-pyn-space
   PYN_S3_ACCESS_KEY_ID_FILE=/run/secrets/spaces_key_id
   PYN_S3_SECRET_ACCESS_KEY_FILE=/run/secrets/spaces_secret
   ```

   Use `https://<region>.digitaloceanspaces.com` as the endpoint (without the Space's name: with the default
   virtual-host addressing the server puts the bucket in front of the host itself). Keep `PYN_S3_PATH_STYLE` unset for
   Spaces; set it to `true` for MinIO and other servers that cannot resolve `<bucket>.<host>`. `PYN_S3_PREFIX` lets
   several installations share one Space.
3. Start the copies. Each one writes, reads back and deletes a marker object before it serves, so a wrong key, an
   unreachable endpoint or a missing Space stops it at start with the failed operation and status in the message
   (`S3 PutObject failed: HTTP 403 ...`). `GET /v1/admin/settings` (as an administrator) shows `storage` with the kind,
   the bucket and `ok` or `degraded`; `GET /healthz` stays a plain liveness check so a store outage does not make a load
   balancer drop every copy.

What the provider's behaviour means for you (details in [provider notes](concepts/storage.md#provider-notes)):

- Spaces is the first hosted target but has not yet been confirmed against a real Space; run the opt-in suite described
  in [storage](concepts/storage.md#tests) with a throwaway Space before relying on it.
- Throttling (`429`, `SlowDown`) and `5xx` answers are retried with backoff (`PYN_S3_MAX_RETRIES`), so a provider's
  request-rate limits show up as slower pushes until the retries run out, then as a `storage_error`.
- Content over 5 GiB goes up by multipart, whose parts must be at least 5 MiB; an upload cut off by a crash leaves parts
  that count against your quota until aborted, so add a lifecycle rule that aborts incomplete multipart uploads.
- A bucket policy that hides missing keys behind `403` (no list permission, as on AWS S3) makes the store report errors;
  grant list permission on the bucket.
- The spool directory (`PYN_S3_SPOOL_DIR`) must hold the largest object being uploaded; in the container image `/tmp` is
  memory-backed, so point it at a volume when large files are expected.

### Moving the object store

`pyn-server migrate-objects` copies every stored object and chunk manifest from a data directory into a bucket, or from a
bucket back into a directory, so an existing install can change store without losing history. It runs on the host, next to
`recover-admin`, and exits.

```
pyn-server migrate-objects (--from-data-dir PATH | --from-s3) (--to-data-dir PATH | --to-s3)
                           [--dry-run] [--verify] [--jobs N] [--force]
```

- A side is either a data directory (`--from-data-dir` / `--to-data-dir`, the path given explicitly; `PYN_DATA_DIR` is not
  read) or the bucket (`--from-s3` / `--to-s3`) described by the `PYN_S3_*` variables, with the same `_FILE` variants as the
  server. Exactly one side is the bucket for a move between a directory and a bucket; two directories also work. Both sides
  the bucket is refused, as is the same directory (or one inside the other).
- `--dry-run` reads and compares but writes nothing (no directory is created, no marker is written to the bucket) and
  prints what would be copied.
- `--verify` re-reads every object and manifest it copied from the destination and compares hashes, failing the run on any
  difference. Objects already present are always compared by size and hash before being skipped, so a run with or without
  `--verify` never trusts what is there.
- `--jobs N` (1 to 64, default 4) is the number of objects handled at once.
- `--force` runs although a server answers on `PYN_ADDR` (default `127.0.0.1:7878`). Without it the command refuses, because
  chunk and manifest writes from a live server would interleave with the copy. That check only sees a server reachable on
  that address from this host; stopping every copy, on every host, is up to the operator.
- Progress lines (counts and bytes, never credentials) go to standard error. The summary goes to standard output:
  `copied` (or `would copy`), `skipped`, `verified` with `--verify`, and `failed`, followed by the failing keys (the first
  20, then a count). The exit status is 0 when nothing failed and 1 otherwise, including a failure to reach either store.
- The command never deletes from either side. Run again after any failure: what is already in the destination is skipped,
  so a run can be interrupted and resumed, and a run right after a clean one copies nothing. An object that exists in the
  destination with different content is reported, not overwritten; remove it there by hand and run again.

Cutover from a data directory to a bucket:

1. Stop every copy of the server, so nothing writes during the copy. Back up first (`scripts/backup.sh`).
2. Set `PYN_S3_*` in the shell, then check the plan:
   `pyn-server migrate-objects --from-data-dir /data --to-s3 --dry-run`.
3. Copy with verification: `pyn-server migrate-objects --from-data-dir /data --to-s3 --verify`. In the container image,
   `docker compose run --rm -e PYN_S3_ENDPOINT=... server migrate-objects --from-data-dir /data --to-s3 --verify`
   with the data volume mounted.
4. Run it once more: it should report `copied: 0` and `failed: 0`.
5. Switch the configuration: remove `PYN_DATA_DIR` and set `PYN_S3_*` (the server refuses both together), then start the
   server. Check `GET /v1/admin/settings` shows `storage` kind `s3` and clone a repository.
6. Keep the old directory until you have used the new store for a while; it is untouched and the way back
   (`--from-s3 --to-data-dir /data`) works the same way.

Moving to a bucket needs list permission on it (the command lists keys), and a bucket whose objects were also written by
something else is copied as it is: only keys in pyn's layout are listed.

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
standard output (messages go to standard error). The account's sessions and tokens end, and so do its [two-factor](concepts/access.md#two-factor-authentication) factors and recovery codes, so a lost authenticator cannot keep the administrator out (another account's factors are reset through the API by an administrator, see [requiring two-factor authentication](concepts/access.md#requiring-two-factor-authentication)); its SSH keys and linked external
identities are left as they are. Other accounts are not touched. When the administrator who left may still hold credentials, recover into a new account name instead of reusing theirs, then disable the old account once you can sign in: recovery does not touch an account's SSH keys or linked identities. It refuses an organization name, the in-memory store (no
`PYN_DATABASE_URL`), and a server that has not run [first-run setup](concepts/access.md#first-run-setup) yet.

Each recovery is written to the server audit log (`GET /v1/admin/audit`) as `admin_granted`, plus `account_enabled` when
it lifted a disable, with the actor `@host-recovery`, and to the server log as a `host recovery` warning.

The server warns once at start when exactly one active administrator exists. Add a second one (`pyn admin grant <user>`) so a single lost account does
not need this; the installer and `pyn setup` suggest it, and `GET /v1/admin/settings` reports `administrators.single` for a client to warn.

### Lost secret key

`PYN_SECRET_KEY` encrypts two things at rest in the database: the signing secrets of [webhooks](concepts/webhooks.md#encryption-at-rest) and the seeds of [two-factor](concepts/access.md#two-factor-authentication) authenticator apps. Losing it loses only those; accounts, passwords, tokens, history and files do not depend on it. Without
the key the server still starts, but deliveries to webhooks whose secrets were encrypted fail until they are replaced, and an account whose authenticator seed was encrypted cannot sign in with an app code (its recovery codes still work, and `recover-admin` clears an administrator's factors). To
recover the webhooks: set a new `PYN_SECRET_KEY` (`openssl rand -hex 32`), then for each webhook run `pyn admin webhook rotate <id>`
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
re-encrypts every stored webhook secret and two-factor seed, retired ones and plaintext ones included, (the seeds first, then the webhook secrets, each kind in one database transaction): either all of a kind are
rewritten or, if any cannot be decrypted with the current key, none, and it says so; a wrong current key therefore changes nothing. Run it once; afterwards the old key no
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

# Putting a load balancer in front

How to run several copies of the bundled stack behind any load balancer: the layouts, what the copies must share, health checks, draining, timeouts, client addresses and TLS, with a worked example on DigitalOcean. What is and is not safe with several copies is in [scale-out](concepts/scale-out.md); the single-node setup, HTTPS and stopping are in [deployment](deployment.md). Provider-specific names and defaults below are marked "check the provider's documentation" where they are not something this repository can verify.

## Layouts

The reference layout puts the balancer in front of the web containers, which proxy the API:

```
browsers, the pyn command
        |  https
   load balancer  (terminates TLS)
        |  http
   N web containers  (PYN_TLS=off, port 8080)
        |  http
   N servers  (port 7878)
        |
   one PostgreSQL     one S3-compatible bucket
```

- Each web container serves the app and proxies `/v1`, `/healthz` and `/readyz` to the server in `PYN_API_URL` ([the image](https://github.com/avalon-initiative/pyn-web/blob/main/docs/image.md#several-servers)). `PYN_API_URL` holds one address, or several separated by spaces or commas (one scheme, no path): the web container then balances by least connections, health-checks each server's `/readyz`, and retries only when a connection could not be made, so a push or upload that reached a server is never replayed to another. A name that resolves to several addresses is not expanded; list each server. Either one web container per server or one web container in front of several servers works.
- `PYN_TLS=off` makes the web container listen for plain HTTP on 8080, so the balancer holds the certificate ([TLS](#tls)).
- All servers share one PostgreSQL database and one bucket, and the [settings below](#what-must-be-identical-across-copies).

The simpler layout leaves out the web containers, for a web app served from somewhere else (a static host, a CDN, another stack):

```
the pyn command, a web app on another origin
        |  https
   load balancer
        |  http
   N servers (port 7878)
```

The web app's origin is then listed in every server's `PYN_ALLOWED_ORIGINS` ([deployment](deployment.md#the-web-app)). The server listens on `PYN_ADDR` (the image sets `0.0.0.0:7878`); the bundled Compose files publish it on loopback only, so a balancer on another host needs the port reachable on a private network and never on the internet. Both layouts need the server's port reachable by the balancer only.

## No sticky sessions

Any copy can answer any request, so the balancer needs no session affinity and may use round robin or least connections. Everything a request depends on is in the database:

- Web sessions and their CSRF tokens are rows in `sessions`; a token made on one copy is accepted by another.
- Locks and leases are one atomic upsert in `locks`, and a lock held through one copy is refused on another.
- Lock changes are read from the database, so a lock stream held open on one copy wakes for a lock taken on another (polled twice a second).
- OpenID Connect sign-in flows are single-use rows, so the callback may land on any copy.
- Resumable uploads continue on any copy; content is in the shared bucket.

`crates/pyn-server/tests/two_copies.rs` and `crates/pyn-cli/tests/two_copies.rs` check each of these from outside with two real servers on one database and one bucket ([the list](concepts/scale-out.md#proven-by-a-test-with-two-copies)). Affinity does no harm, but it is not needed.

## What must be identical across copies

| Setting | Why |
| --- | --- |
| `PYN_SECRET_KEY` | Encrypts webhook signing secrets and two-factor seeds at rest. A copy with another key cannot read what the others wrote, so sign-ins with two-factor and webhook deliveries fail on that copy only |
| `PYN_SETUP_TOKEN` | Until first-run setup is done. An unset token is generated per process, so each copy would print a different one. Ignored once set up |
| `PYN_INSTANCES` | The number of copies, above 1. The server then refuses to start without `PYN_DATABASE_URL` and shared content storage |
| `PYN_DATABASE_URL` | The same PostgreSQL database |
| `PYN_S3_BUCKET` and its endpoint, region and keys (or one shared `PYN_DATA_DIR`) | The same content storage ([several copies on a bucket](deployment.md#several-copies-on-a-bucket)) |
| `PYN_PUBLIC_URL` | The balancer's https address, for the `Secure` cookie flag, links and sign-in origin checks |
| `PYN_TRUST_FORWARDED_FOR` and `PYN_TRUSTED_PROXIES` | The same values everywhere ([client addresses](#client-addresses)) |

Keep clocks in sync (NTP). `PYN_WEBHOOK_WORKER=false` keeps a copy out of webhook delivery, but at least one copy must deliver.

## Health checks

The server has two checks. Neither needs credentials, and both answer before setup.

| Path | Question | Use it for |
| --- | --- | --- |
| `GET /healthz` | Is the process up? Never looks at the database or the bucket | Container health, a restart-on-failure supervisor, a Kubernetes `livenessProbe` and `startupProbe` |
| `GET /readyz` | Should this copy get traffic? | The balancer's health check, a Kubernetes `readinessProbe` |

`/readyz` returns `200` with `status: "ok"` when the metadata store answered a cheap query (a 2 second limit, the answer reused for 3 seconds) and the object store has not failed its probe (about every 30 seconds for a bucket). It returns `503` with `status: "unavailable"` and the failing check named in `checks` (`metadata_store` or `object_store`) when either fails, and `503` with `status: "draining"` once the copy got a stop signal. Source: `crates/pyn-server/src/ready.rs`. A bucket outage therefore marks the copy unready within about 30 seconds, and a database outage within a few seconds. When the database or bucket is down for every copy, every copy is unready at once: that is correct, so set the balancer to show its own error page or to keep serving when none is healthy, as the provider allows.

The web container proxies `/readyz` like `/healthz`, without caching. Through it the answer is a server's real state: with one server in `PYN_API_URL` it is that server's own answer, including its `503`; with several it is whichever server the web container picked, and the web container answers `503` itself only when every server is out. A balancer that checks each server individually should still check the server's own `/readyz` on its port, from the balancer's private address (many balancers can check a port other than the traffic port; check the provider's documentation); where one cannot, `/readyz` on the web container's port 8080 is a usable check. Web images older than the one that added this proxy answer `/readyz` with the app's page, which always passes.

Settings to start from, then tune:

| Setting | Value |
| --- | --- |
| Path | `/readyz`, expecting `200` (the body is JSON; do not match on it) |
| Interval | 5 to 10 seconds |
| Timeout | 3 seconds, above the 2 second metadata limit |
| Unhealthy threshold | 2 to 3 consecutive failures |
| Healthy threshold | 2 consecutive successes |

A copy that is draining answers `503 draining` to every probe for as long as `PYN_SHUTDOWN_DRAIN_DELAY_SECS` lasts, then closes its listener, so a new probe gets a refused connection. Both count as failures. With the default delay of 0 the listener closes at once, see [draining](#draining-and-the-stop-grace-period).

## Draining and the stop grace period

On `SIGTERM` or `SIGINT` the server marks itself draining (`/readyz` answers `503 draining`), keeps accepting connections for `PYN_SHUTDOWN_DRAIN_DELAY_SECS` (default 0, whole seconds, at most 3600), then stops accepting and lets running requests finish for `PYN_SHUTDOWN_GRACE_SECS` (default 30, whole seconds). The grace counts from the moment the listener closes, so a stop takes at most delay plus grace. Lock streams end at once, at the start of the delay, with the page they have (`pyn locks --watch` reconnects from its cursor), and the process exits `0`, or `1` if requests were cut off. A second signal during the delay skips the rest of it. During the delay every request other than the readiness probe is served in full and counts as in flight. The sequence is in [stopping and restarting](deployment.md#stopping-and-restarting).

With the default delay of 0 the listener closes at once, so a request the balancer sends before it notices fails with a refused connection. Set the delay instead of sleeping in the runtime:

1. Set `PYN_SHUTDOWN_DRAIN_DELAY_SECS` a little above the balancer's health check detection time: interval times unhealthy threshold (5 seconds times 3 failures is 15, so 20). The copy fails its checks while it still answers, the balancer takes it out, and no request meets a closed listener.
2. Set `PYN_SHUTDOWN_GRACE_SECS` to at least the longest push or upload that should be allowed to finish.
3. Give the runtime a stop timeout longer than delay plus grace: Compose `stop_grace_period` (the deploy files set `45s`, which covers the default 30 second grace and no delay, so raise it when you set a delay), Kubernetes `terminationGracePeriodSeconds` (default 30), or systemd `TimeoutStopSec` (90 by default).

What remains true: the delay does not remove the need for the balancer to act on failed checks (a balancer that ignores readiness keeps sending until the listener closes), and a balancer's own deregistration delay (connection draining) still decides how long it keeps sending requests it already routed. If the balancer or an orchestrator takes the copy out of rotation before sending the signal (a Kubernetes Service removing the pod's endpoint, or deregistering the copy yourself), a delay is still harmless. Kubernetes removes the pod from a Service's endpoints and sends `SIGTERM` at about the same time, so the delay replaces the `preStop` sleep:

```yaml
terminationGracePeriodSeconds: 60
containers:
  - name: pyn-server
    env:
      - name: PYN_SHUTDOWN_DRAIN_DELAY_SECS
        value: "20"
      - name: PYN_SHUTDOWN_GRACE_SECS
        value: "30"
```

Restart copies one at a time so the others keep serving. A cut-off upload is retried by the client, and no partial state is committed.

## Timeouts

| Balancer setting | Recommendation | Reason |
| --- | --- | --- |
| Idle or read timeout, at least | 30 seconds | A lock stream request (`GET /v1/repos/{owner}/{name}/locks/events?after=...&wait=...`) is held open with no bytes sent for at most 25 seconds: `MAX_WAIT_SECS` in `crates/pyn-server/src/lock_stream_api.rs` caps `wait` at 25, and the server checks the database every `POLL_EVERY` (500 ms), so the answer comes within about 25.5 seconds. The `pyn` command asks for the 25 second maximum (`HOLD_SECS`, `crates/pyn-cli/src/lock_watch.rs`) |
| Idle or read timeout, in practice | 120 seconds or more | The server's own limit for a read route is `PYN_REQUEST_TIMEOUT_SECS`, default 60 (`DEFAULT_REQUEST_TIMEOUT`, `crates/pyn-server/src/request_limits.rs`), and it answers `503 request_timeout` itself. A balancer timeout below 60 turns that into the balancer's own gateway error. The lock stream is one of these routes, so the setting must also stay above 25 |
| Read timeout for pushes and uploads | 300 seconds | The web container waits up to 300 seconds on the server for reads and writes (`read_timeout` and `write_timeout`, `docker/caddy/Caddyfile` in `pyn-web`). Assembling a large object or finishing a push can send nothing back for a while after the body is in |
| Request body size | No limit | Pushes and uploads send large bodies, and the web container sets no limit either. A body limit (some providers and CDNs have one, for example Cloudflare's 100 MB on the lower plans) cuts them off |
| Request buffering | Off | The web container does not buffer requests (`request_buffers 0`) and streams responses (`flush_interval -1`); a balancer that buffers a whole body before forwarding it adds delay and needs disk or memory for it |
| Upload body idle time | At or above 30 seconds | The server fails an upload body that sends nothing for `PYN_BODY_IDLE_TIMEOUT_SECS` (default 30), so slow clients are cut by the server first |
| Protocol to the backend | HTTP/1.1 with keep-alive | Nothing needs HTTP/2 to the backend |
| Stop timeout of the runtime | Above `PYN_SHUTDOWN_DRAIN_DELAY_SECS` plus `PYN_SHUTDOWN_GRACE_SECS` | The runtime kills the process when it ends, cutting off whatever still runs ([draining](#draining-and-the-stop-grace-period)) |
| Deregistration delay, when the balancer has one | Above the longest request it already routed | It is separate from the server's delay; the server's delay covers the time the balancer needs to notice failed health checks |

The API uses plain request and response HTTP. There is no WebSocket or server-sent events endpoint: the lock stream is a long poll, so a balancer needs no upgrade handling. If `PYN_REQUEST_TIMEOUT_SECS` is set below about 26, the server itself cuts lock streams short with `503 request_timeout`, so leave it at 60 or more.

## Client addresses

The server uses the client address for per-client rate limits, signed-out directory limits and the source ranges of service credentials. With `PYN_TRUST_FORWARDED_FOR=true` it takes the address from the `X-Forwarded-For` header (`address` in `crates/pyn-server/src/client.rs`): the entry `PYN_TRUSTED_PROXIES` positions from the right (default 1, the last entry; 1 to 16), after combining several header lines in order and trimming each entry. It falls back to the connection's peer when the header is absent, has fewer entries than the setting, or that entry is empty. `PYN_TRUSTED_PROXIES` without `PYN_TRUST_FORWARDED_FOR=true` stops the server at start. `X-Forwarded-Host` and `X-Forwarded-Proto` are also trusted for LFS URLs; they are not counted by hops.

Set it to `true` only when every request arrives through a proxy that sets the header; anyone who can reach the server directly can send any value.

**Reference layout.** The web container sends the server exactly one `X-Forwarded-For` entry, the client, and replaces any value it received (`header_up X-Forwarded-For {client_ip}` in its Caddyfile). Caddy reads the client from the connection's peer unless the peer is in `PYN_TRUSTED_PROXY_RANGES`; for a listed proxy it reads `CF-Connecting-IP`, else `X-Forwarded-For` from the right, taking the first address that is not listed. So with the balancer in front, the web container sees the balancer as the peer: unless its addresses are listed, every client looks like the balancer and shares one set of limits. Set `PYN_TRUSTED_PROXY_RANGES` on the web containers to the balancer's addresses (space or comma separated; never `private_ranges`, `0.0.0.0/0` or `::/0`, which the container refuses) and the balancer must put the client in `X-Forwarded-For`, which load balancers do. Set `PYN_TRUST_FORWARDED_FOR=true` on the servers (the Compose files already do).

When Cloudflare proxies to the balancer, the client comes from `CF-Connecting-IP`, which a listed proxy is trusted to set. That is safe only when the balancer accepts connections from Cloudflare alone; otherwise a client can send the header itself. [Public address and client addresses per layout](deployment.md#public-address-and-client-addresses-per-layout) lists the other cases.

**Servers straight behind the balancer.** The last entry must be the real client, so the balancer must append the connecting address to `X-Forwarded-For`, not pass a client-supplied value through unchanged. Check the provider's documentation for how it treats the header. With a CDN in front of the balancer, both appending, the header ends `client, cdn`: set `PYN_TRUSTED_PROXIES=2` so the client is the second entry from the right. Count only proxies you know append the address they saw (a hop that replaces the header does not add an entry), and only accept connections from the CDN at the balancer; otherwise a client can put its own entries in the list. The web container replaces the header with one entry, so behind it the default `1` stays right whatever is in front of the container. [Layouts and values](deployment.md#public-address-and-client-addresses-per-layout).

## TLS

Three choices:

1. **Terminate at the balancer** (the common case). The web containers run `PYN_TLS=off` and speak plain HTTP on 8080 to the balancer, on a private network; the servers' `PYN_PUBLIC_URL` is the balancer's `https://` address, so cookies are `Secure` and sign-in origin checks match. Clients reach one https address.
2. **Terminate in the web container.** Leave `PYN_TLS` at its mode and `PYN_SITE` set; the balancer then passes TCP through (a layer 4 balancer) and does not see HTTP. The web container reads the client from the connection, which a TCP balancer hides unless it preserves the source address; the health check must then use the server's port directly as above. Each container needs its own certificate and renews it itself, and `auto` needs ports 80 and 443 reachable from the internet on the name's DNS, so this suits few cases.
3. **A managed certificate at the balancer.** The provider issues and renews it for a name in a zone it can validate. No certificate files, no `pyn-tls` volume, no renewal to watch. Check the provider's documentation for the validation it needs.

The redirect from http to https is the balancer's to do in the first two cases; with `PYN_TLS=off` the web container does not redirect. Passkeys need a host name over https ([why](deployment.md#why-passkeys-need-a-host-name)): the balancer's name is the one to choose before people register.

## Sizing and what scales

- Servers scale out: add copies, bump `PYN_INSTANCES` on all of them, and they share state through the database. The web containers hold no state and scale the same way.
- PostgreSQL and the bucket are the shared limits. Every copy opens its own connections to the database, so the sum across copies must stay below its connection limit; a managed service's limits are in the provider's documentation. A bucket's request rates are the provider's; the server retries throttling and `5xx` with backoff, which shows up as slower pushes ([several copies on a bucket](deployment.md#several-copies-on-a-bucket)).
- Some limits are per process on purpose, because they bound one process's memory or CPU, so N copies allow about N times the value: the concurrent upload caps `PYN_MAX_UPLOADS_PER_USER` and `PYN_MAX_UPLOADS` ([limits](concepts/limits.md#concurrent-uploads)), the cap on expensive requests `PYN_MAX_CONCURRENT_EXPENSIVE`, and the merge cap `PYN_MAX_CONCURRENT_MERGES`. Size these per copy, not per cluster. The sign-in, registration and service credential rate limits are in the database already and are shared.
- Webhook delivery concurrency (`PYN_WEBHOOKS_CONCURRENCY`) is per delivering copy.

## Example: DigitalOcean

A load balancer with a managed certificate in front of two droplets (or a Kubernetes cluster), a managed PostgreSQL cluster and a Spaces bucket, in the reference layout. These are the settings, not a walk through the control panel; names of options, defaults and limits change, so check the provider's documentation for each.

1. **Network.** Put the droplets, the database and the load balancer in one VPC. Only the balancer is reachable from the internet. Allow the balancer to reach the droplets on 8080 (web) and 7878 (server health check); allow the droplets to reach the database.
2. **PostgreSQL.** Create a managed cluster and a database for pyn; restrict its trusted sources to the droplets. Use its connection string with TLS as `PYN_DATABASE_URL` (check its documentation for the parameters it requires). Size the cluster's connection limit for all copies together.
3. **Spaces.** Create a private Space and an access key for it, and set the `PYN_S3_*` variables as in [several copies on a bucket](deployment.md#several-copies-on-a-bucket). Add the lifecycle rule that aborts incomplete multipart uploads.
4. **Each droplet** runs a server and a web container. The server: the image `ghcr.io/avalon-initiative/pyn-core:<tag>` with `PYN_ADDR=0.0.0.0:7878` on the private address only, `PYN_INSTANCES=2`, the same `PYN_SECRET_KEY` and `PYN_SETUP_TOKEN` on both, the database and Space settings, `PYN_PUBLIC_URL=https://pyn.example.com`, `PYN_TRUST_FORWARDED_FOR=true`, `stop_grace_period: 45s`. The web container: `ghcr.io/avalon-initiative/pyn-web:<tag>` with `PYN_TLS=off`, `PYN_API_URL=http://<this droplet's server>:7878` and `PYN_TRUSTED_PROXY_RANGES` set to the balancer's private addresses (the VPC address the balancer connects from; check the provider's documentation for what it is). The `deploy/` files cover the pieces: `docker-compose.server.yml` and `docker-compose.web.yml` with `PYN_WEB_PUBLISH` set to the private address and port.
5. **Load balancer.**
   - Forwarding rule: HTTPS on 443 to HTTP on the droplets' 8080, with a certificate the provider manages for the name (a domain in DigitalOcean DNS is the usual way to let it validate; check the documentation). Redirect HTTP on 80 to HTTPS if the option exists.
   - Health check: HTTP, path `/readyz`, on the server's port 7878 if the balancer can check a different port than it forwards to (check the documentation), with the interval, timeout and thresholds in [health checks](#health-checks). If it cannot, use `/readyz` on 8080 through the web container.
   - Sticky sessions: off. Algorithm: round robin or least connections.
   - Timeouts: set the idle or HTTP keepalive timeout as in [timeouts](#timeouts). Check the documentation for what it can be set to; if its maximum is below 30 seconds, lock streams will be cut and `pyn locks --watch` will reconnect repeatedly.
   - Forwarded headers: confirm the balancer sends the client in `X-Forwarded-For`, and read in its documentation what it does to a value the client sent.
6. **DNS.** Point the name at the balancer. Then first-run setup through the balancer's address with the shared `PYN_SETUP_TOKEN`.
7. **Deploys.** For each droplet in turn: remove it from the balancer (or disable its health check path), wait for in-flight requests, stop and update the containers with `docker compose up -d`, wait for `/readyz` to answer `200`, add it back.

On Kubernetes the same settings apply: a Deployment for the servers with `/healthz` as the `livenessProbe` and `startupProbe` and `/readyz` as the `readinessProbe`, the termination settings in [draining](#draining-and-the-stop-grace-period), a Service for the web containers, and a Service of type LoadBalancer or an Ingress whose annotations set the certificate, health check and timeouts (the annotation names are the provider's; check its documentation).

A Compose file with two servers behind one web container is not given here: set `PYN_API_URL` on the web service to both servers' addresses (for example `http://server1:7878 http://server2:7878`).

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| Sign-in works on some requests and fails on others, or two-factor and webhook secrets fail on one copy | The copies have different `PYN_SECRET_KEY` values | Set the same key everywhere and restart; secrets written under another key stay unreadable until re-entered |
| Everyone shares one client address; a handful of users hit rate limits together | The web container or the server takes the balancer as the client: `PYN_TRUSTED_PROXY_RANGES` does not list the balancer, or the servers are behind a proxy that does not append to `X-Forwarded-For` | List the balancer's addresses on the web containers; with servers directly behind the balancer, make it append the client as the last entry, and set `PYN_TRUSTED_PROXIES` to the number of appending proxies when there are several ([client addresses](#client-addresses)) |
| Uploads or pushes fail after a fixed number of seconds | The balancer's idle, read or request-duration timeout, or a body size limit | Raise the timeouts to 300 seconds, remove the body limit, turn off request buffering ([timeouts](#timeouts)) |
| `pyn locks --watch` reconnects every few seconds | The balancer's idle timeout is below the 25 second lock stream wait | Raise it above 30 seconds, preferably 120 |
| Copies flap between ready and unready | The metadata store is slow or down, or the bucket probe fails (`/readyz` names the check) | Look at the `checks` in the `/readyz` body, then the database connection limit or the bucket's credentials and endpoint; `/healthz` stays `200` throughout |
| `502` during deploys | The balancer sends requests to a copy that already closed its listener, or the runtime killed the process before the grace period ended | Set `PYN_SHUTDOWN_DRAIN_DELAY_SECS` above the balancer's health check interval times its unhealthy threshold, and a stop timeout above delay plus `PYN_SHUTDOWN_GRACE_SECS`; a balancer that ignores `/readyz` still needs deregistering first ([draining](#draining-and-the-stop-grace-period)) |
| Web app loads but sign-in is refused with a cross-origin error | `PYN_PUBLIC_URL` is the old `http://` address, or the web app's origin is not in `PYN_ALLOWED_ORIGINS` | Set `PYN_PUBLIC_URL` to the balancer's https address on every server |
| A `/readyz` check always passes, or does not show the database failing | The web image is older than the one that proxies `/readyz`, so the path is answered with the app's page | Update the web image, or point the check at the server's port |

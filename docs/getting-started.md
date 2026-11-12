# Getting started

Run a pyn server from a release, set it up in the browser and connect the `pyn` command to it. You need a computer or
virtual machine with [Docker](https://docs.docker.com/engine/install/) (with the Compose plugin: `docker compose version`
should print a version) and about 2 GB of free memory. Provisional: written against the first pre-release, `v0.1.0-alpha`.

What you end up with: by default the server, the web app and a PostgreSQL database, started by one Compose file that takes
four settings. Pick the shape that fits (each is one self-contained Compose file, so no extra flags):

| You want | Download | Env file | Runs |
| --- | --- | --- | --- |
| Server and web app together (the usual case) | `docker-compose.yml` | `.env.example` | PostgreSQL, server, web app |
| The API only, no web app | `docker-compose.server.yml` | `.env.server.example` | PostgreSQL, server |
| Only the web app, against a server hosted elsewhere | `docker-compose.web.yml` | `.env.web.example` | web app |
| Only the `pyn` command, against an existing server | nothing | nothing | go to [step 6](#6-install-the-pyn-command) |

Step 3 uses the first row; the other rows change only the two downloads, see [Other shapes](#other-shapes).

## 1. Find the release

Open the [releases page](https://github.com/avalon-initiative/pyn/releases) and pick a release. Two names matter:

- the **version**, the first part of the tag: for the tag `v0.0.2-alpha` it is `v0.0.2` (the images are tagged with the
  version only);
- the **tag**, the whole name (`v0.0.2-alpha`), which you use to download the Compose file.

## 2. Let Docker pull the images

The images are `ghcr.io/avalon-initiative/pyn-core` (server) and `ghcr.io/avalon-initiative/pyn-web` (web app). Try a
pull first (use your version):

```bash
docker pull ghcr.io/avalon-initiative/pyn-core:v0.0.2
```

If it downloads, skip to step 3. If you get `denied` or `unauthorized`, the packages are not public yet. Sign in to the
GitHub container registry with a personal access token:

1. On GitHub open **Settings → Developer settings → Personal access tokens → Tokens (classic) → Generate new token
   (classic)**.
2. Give it a name, tick only **`read:packages`**, and create it. Copy the token (GitHub shows it once).
3. Run `docker login ghcr.io -u <your GitHub user name>` and paste the token at the password prompt.

This GitHub token is only for pulling images. It has nothing to do with the *setup token* in step 5.

## 3. Download the Compose file and fill in the settings

```bash
mkdir pyn && cd pyn
TAG=v0.0.2-alpha        # the tag from step 1
curl -fsSL https://raw.githubusercontent.com/avalon-initiative/pyn/$TAG/deploy/docker-compose.yml -o docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/avalon-initiative/pyn/$TAG/deploy/.env.example -o .env
```

Fill in the generated secrets and the version (the version is the tag without its suffix, as in step 1):

```bash
sed -i "s/^PYN_VERSION=.*/PYN_VERSION=v0.0.2/" .env
sed -i "s/^POSTGRES_PASSWORD=.*/POSTGRES_PASSWORD=$(openssl rand -hex 24)/" .env
sed -i "s/^PYN_SECRET_KEY=.*/PYN_SECRET_KEY=$(openssl rand -hex 32)/" .env
```

On macOS use `sed -i ''` instead of `sed -i`. Then edit `.env` for your network:

| Setting | What to put |
| --- | --- |
| `PYN_PUBLIC_URL` | The address you will type in the browser, for example `http://192.168.1.20:8080` or `https://pyn.example.com`. Keep `http://localhost:8080` if you only use this computer. |
| `POSTGRES_VERSION` | Optional. The PostgreSQL major version, `16` unless set. A database created on one major version cannot be opened by another, so change it only with a [dump and restore](deployment.md#backups). |
| `PYN_WEB_PUBLISH` | Remove the `#` and set `0.0.0.0:8080` so other computers can reach it. Leave it commented to keep the server on this computer only. |

`.env` holds secrets and must not be committed or shared. Copy `PYN_SECRET_KEY` somewhere safe: webhook secrets stored in
the database cannot be read without it.

## 4. Start it

```bash
docker compose up --detach --wait
```

The first start pulls the images and takes a minute or two. It returns when the database, server and web app report
healthy. If it fails, `docker compose logs` shows why; `required variable ... is missing` means a setting in `.env` is
empty.

### Other shapes

Download the files of your row and save them as `docker-compose.yml` and `.env`, so every later command is the plain
`docker compose ...` used in this guide. Replace the two `curl` lines of step 3 with:

```bash
# server only
curl -fsSL https://raw.githubusercontent.com/avalon-initiative/pyn/$TAG/deploy/docker-compose.server.yml -o docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/avalon-initiative/pyn/$TAG/deploy/.env.server.example -o .env

# web app only
curl -fsSL https://raw.githubusercontent.com/avalon-initiative/pyn/$TAG/deploy/docker-compose.web.yml -o docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/avalon-initiative/pyn/$TAG/deploy/.env.web.example -o .env
```

**Server only.** Fill in `PYN_VERSION`, `POSTGRES_PASSWORD` and `PYN_SECRET_KEY` as above. `PYN_PUBLIC_URL` is the
server's own address, for example `http://192.168.1.20:7878` (the API listens on port 7878; set `PYN_SERVER_PUBLISH=0.0.0.0:7878`
to reach it from other computers). Setup (step 5) then happens with the `pyn` command instead of the browser, and step 7
points `pyn` at this address:
`PYN_SETUP_TOKEN="<token>" pyn --server http://192.168.1.20:7878 setup admin --public-url http://192.168.1.20:7878`.
A web app can be added later: run the web-only file elsewhere (next paragraph).

**Web app only.** Fill in `PYN_VERSION` and `PYN_API_URL`, the address of the server this web app talks to (for example
`https://pyn.example.com`). There is no database and no secret; `docker compose up --detach --wait` starts just the web
app, on `127.0.0.1:8080` unless `PYN_WEB_PUBLISH` says otherwise. Signing in checks the origin the browser loaded the page
from, so the server must be told this web app's address: set `PYN_ALLOWED_ORIGINS` on the remote server to it, with the
scheme and port and no path (`http://localhost:8080` for a web app on your own computer; several are comma-separated), and
restart that server. Without it sign-in fails with `403 origin_not_allowed`. See
[access](concepts/access.md#web-sessions) and [configuration](configuration.md).

## 5. Set up the server (first administrator)

A new server serves only setup until the first administrator exists. To stop a stranger from claiming it first, setup
needs a one-time **setup token**, printed in the server's log at first start:

```bash
docker compose logs server | grep -A4 "one-time setup token"
```

The block looks like this, followed by one line saying where to finish setup:

```text
----- pyn one-time setup token -----
<the token>
------------------------------------
finish setup in the browser at https://pyn.example.com, or run: pyn setup <username>
```

Copy the long string between the dashes. Then:

1. Open the address from `PYN_PUBLIC_URL` in your browser. The setup screen appears.
2. Enter the setup token, then the administrator's user name, password and email (optional).
3. Choose who can create accounts: **invite** (the default: only people you invite), **closed** or **open**.
4. Finish the screen. You are signed in as the administrator, and the token stops working.

The token is printed once. If you missed it, set `PYN_SETUP_TOKEN` to a string of your own (at least 16 characters) in
`.env` and run `docker compose up -d`. Remove it from `.env` after setup; the server warns while it is still set.

Check the server any time with `curl http://localhost:8080/healthz`.

## 6. Install the `pyn` command

On the release page, download the CLI archive for your computer (`pyn-cli-<tag>-linux-x86_64.tar.gz`,
`pyn-cli-<tag>-linux-arm64.tar.gz` or, for macOS on Apple silicon, `pyn-cli-<tag>-macos-arm64.tar.gz`), check it and
put `pyn` on your `PATH`. (The `pyn-server-` archives are only for running the server without Docker.)

```bash
sha256sum --check --ignore-missing SHA256SUMS      # macOS: shasum -a 256 -c SHA256SUMS --ignore-missing
tar xzf pyn-cli-*.tar.gz
sudo install pyn-cli-*/pyn /usr/local/bin/pyn          # or any folder on your PATH
pyn --version
```

There are no Windows builds yet.

## 7. Connect and try it

Point the command at the server once, then sign in. The web app's address works: it forwards the API.

```bash
pyn config set --global server http://192.168.1.20:8080     # your PYN_PUBLIC_URL
pyn login <your user name>
```

Create a repository and take your first lock. A new repository treats every path as exclusive, so a file needs a lock
before it can be changed:

```bash
pyn repo create demo                    # creates <you>/demo
pyn clone <you>/demo && cd demo
echo "hello" > notes.txt
pyn lock notes.txt                      # fails with lock_held if someone else has it
pyn add notes.txt
pyn commit -m "First file"
pyn push                                # publishing releases your lock
```

Paths that can be merged should be `shared` instead of locked: list them in the repository's policy,
[`pyn.toml`](concepts/pyn-toml.md) (`default = "shared"` makes every path shared, like git). To start a repository that
way: `pyn repo create demo --policy shared.toml` with a file containing `[meta]` and `default = "shared"`.

### A second person

With registration set to **invite**, create an invitation for a repository (`pyn invite create --repo <you>/demo --role writer`), and send
the code. They install `pyn`, point it at the same server and run `pyn register <name> --invite <code>`, then
`pyn login <name>` and `pyn clone <you>/demo`. Two people locking the same exclusive file is the quickest way to see
pyn work: the second gets `lock_held` until the first pushes or runs `pyn unlock`.

## Next

- [Deployment](deployment.md): TLS and a reverse proxy, upgrades, backups and restore, several copies.
- [Configuration](configuration.md): every setting.
- [Access](concepts/access.md): roles, invitations and tokens.
- [Releasing](releasing.md): how releases are made.

# Getting started

Run a pyn server from a release, set it up in the browser and connect the `pyn` command to it. You need a computer or
virtual machine with [Docker](https://docs.docker.com/engine/install/) (with the Compose plugin: `docker compose version`
should print a version) and about 2 GB of free memory. Provisional: the first release has just been published.

What you end up with: the server, the web app and a PostgreSQL database, started by one Compose file that takes four
settings.

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

## 5. Set up the server (first administrator)

A new server serves only setup until the first administrator exists. To stop a stranger from claiming it first, setup
needs a one-time **setup token**, printed in the server's log at first start:

```bash
docker compose logs server | grep -A3 "one-time setup token"
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

On the release page, download the archive for your computer (`pyn-<tag>-x86_64-unknown-linux-gnu.tar.gz`,
`...-aarch64-unknown-linux-gnu.tar.gz` or, for macOS on Apple silicon, `...-aarch64-apple-darwin.tar.gz`), check it and
put `pyn` on your `PATH`:

```bash
sha256sum --check --ignore-missing SHA256SUMS      # macOS: shasum -a 256 -c SHA256SUMS --ignore-missing
tar xzf pyn-*.tar.gz
sudo install pyn-*/pyn /usr/local/bin/pyn          # or any folder on your PATH
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

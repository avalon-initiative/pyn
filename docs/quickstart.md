# Quickstart

From nothing to two people hitting a lock, in a few minutes. It runs a server on one machine with Docker and uses the `pyn` command
against it. For a real installation (TLS, backups, upgrades), continue with [Getting started](getting-started.md) and
[Deployment](deployment.md).

You need Docker with the Compose plugin, `curl`, and `openssl`. The `pyn` command is released for Linux (x86_64 and arm64) and
macOS on Apple silicon; there is no Windows build yet, so on Windows use WSL2 with Ubuntu.

## 1. Start a server

```bash
mkdir pyn && cd pyn
TAG=$(curl -fsSL https://api.github.com/repos/avalon-initiative/pyn/releases/latest | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
VERSION=${TAG%%-*}
curl -fsSL https://raw.githubusercontent.com/avalon-initiative/pyn/$TAG/deploy/docker-compose.yml -o docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/avalon-initiative/pyn/$TAG/deploy/.env.example -o .env
sed -i "s/^PYN_SERVER_VERSION=.*/PYN_SERVER_VERSION=$VERSION/" .env
sed -i "s/^PYN_WEB_VERSION=.*/PYN_WEB_VERSION=$VERSION/" .env
sed -i "s/^POSTGRES_PASSWORD=.*/POSTGRES_PASSWORD=$(openssl rand -hex 24)/" .env
sed -i "s/^PYN_SECRET_KEY=.*/PYN_SECRET_KEY=$(openssl rand -hex 32)/" .env
docker compose up --detach --wait
docker compose logs server | grep -A4 "one-time setup token"
```

The server prints a one-time setup token. Open `http://localhost:8080`, enter the token and create the first administrator. If the
web app version is not published for this tag, pick one that is from the
[pyn-web releases](https://github.com/avalon-initiative/pyn-web/releases); the two version independently
(see [compatibility](deployment.md#compatibility)). Every step is explained in [Getting started](getting-started.md#1-find-the-release).

## 2. Install the `pyn` command

Download the CLI archive for the computer from the [releases page](https://github.com/avalon-initiative/pyn/releases)
(`pyn-cli-<tag>-linux-x86_64.tar.gz`, `linux-arm64`, or `macos-arm64`), check it against the published checksum, unpack it and put
`pyn` on the `PATH`. [Getting started](getting-started.md#6-install-the-pyn-command) has the exact commands.

```bash
pyn config set --global server http://localhost:8080
pyn login <your-name>
```

## 3. Make a repository and lock a file

```bash
pyn repo create demo                    # creates <your-name>/demo with the starter policy
pyn clone <your-name>/demo && cd demo
cp ~/some-image.png hero.png            # any binary file
echo "hello" > notes.txt                # any text file
pyn status
```

The starter policy treats text as `shared` and binary kinds (images, models, audio, archives, engine assets) as `exclusive`.
`pyn status` shows each path's mode. The text file can be edited and committed freely. The image is read-only until locked:

```bash
pyn lock hero.png
pyn add hero.png notes.txt
pyn commit -m "First files"
pyn push                                # publishing releases the lock
```

## 4. See the lock work

Create a second user (`pyn register bob`, with registration set to open; see
[the second person](getting-started.md#a-second-person)), add them to the repository, and sign them in from another folder or
machine with a separate config (`PYN_CONFIG_DIR=/tmp/bob pyn login bob`). While the first user holds the lock:

```bash
pyn lock hero.png        # refused with lock_held, naming the holder and when the lease ends
```

The second user cannot edit or commit the file until the first pushes or runs `pyn unlock hero.png`. The lock expires by itself
after its lease (8 hours by default), so an abandoned one never blocks a file forever. Meanwhile both users can change `notes.txt`
and merge their work like in git. `pyn locks` lists who holds what, and `pyn locks --watch` follows changes live.

The same data is in the web app at `http://localhost:8080`.

## Next

- [Shared and exclusive paths](concepts/shared-and-exclusive.md): the model on one page.
- [Using pyn for a game project](engines.md): which paths to lock in Godot, Unity and Unreal.
- [Coming from git, Perforce or Unity Version Control](coming-from.md).
- [Troubleshooting](troubleshooting.md).

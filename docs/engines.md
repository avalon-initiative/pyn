# Using pyn for a game project

Policy and ignore starters for Godot, Unity and Unreal, and the state of editor support. Engine file layouts change between
versions, so treat the lists as starting points and check them against the engine's own documentation.

## State of editor support

pyn has no editor plug-in yet; locks are taken from the command line or the web app. Editor integrations (Godot, Unity, Unreal) are
planned work. What exists today for building one: `--json` output for `status`, `locks`, `lock` and `unlock` and a live
`pyn locks --watch` stream ([CLI JSON output](cli-json.md)), the read-only bit on exclusive files without their lock, and the
Git LFS Batch and Locks API ([Git LFS API](concepts/lfs.md)), which git-based engine tooling already understands.

Large files are stored as content-defined chunks (about 1 MiB average) with deduplication, compression where it pays, and uploads
and downloads that resume ([content storage](concepts/storage.md)). Objects up to 16 GiB are accepted. Partial or sparse workspaces
on a pyn server are not available yet: a clone takes everything.

## Where the ignore rules live

On a pyn server repository the shared ignore file is `.pyn/ignore` (patterns, one per line; see
[the ignore file](concepts/workspace.md#the-ignore-file)). In a git home, the usual `.gitignore` files apply.

## Godot 4

Text resources are mergeable; imported and source binaries are not.

```toml
# .pyn/pyn.toml
[meta]
default = "shared"

[exclusive]
paths = ["**/*.png", "**/*.jpg", "**/*.wav", "**/*.ogg", "**/*.mp3", "**/*.blend", "**/*.glb", "**/*.fbx", "**/*.psd"]

[shared]
paths = ["**/*.tscn", "**/*.tres", "**/*.gd", "**/*.import", "project.godot"]
```

Ignore `.godot/` (the import cache) and exported builds. Commit the `.import` sidecar files next to the sources.

## Unity

Set Edit, Project Settings, Editor: Version Control Mode to Visible Meta Files and Asset Serialization to Force Text. Every asset
has a `.meta` file holding its GUID; a missing or duplicated one breaks references, so keep a `.meta` and its asset together.
Scenes and prefabs are YAML and merge poorly by hand: many teams lock them.

```toml
[meta]
default = "shared"

[exclusive]
paths = [
  "**/*.unity", "**/*.prefab",                       # scenes and prefabs: lock to avoid YAML conflicts
  "**/*.fbx", "**/*.png", "**/*.psd", "**/*.wav", "**/*.mp3",
  "**/*.asset",                                       # large or binary ScriptableObject data
]

[shared]
paths = ["Assets/**/*.cs", "Assets/**/*.shader", "Packages/manifest.json", "ProjectSettings/"]
```

Ignore `Library/`, `Temp/`, `Obj/`, `Logs/`, `UserSettings/`, `Builds/` and IDE folders. Commit `Assets/`, `Packages/` and
`ProjectSettings/`. A lock on an asset does not cover its `.meta`: list `**/*.meta` as exclusive for the locked kinds, or lock
the pair. Automatic grouping and a Unity smart-merge driver are not available yet.

## Unreal Engine

`.uasset` and `.umap` are binary and cannot be merged, so they are exclusive. Commit `Content/`, `Config/`, `Source/` and the
`.uproject`; ignore `Intermediate/`, `Saved/`, `Binaries/`, `DerivedDataCache/` and IDE folders.

```toml
[meta]
default = "shared"

[exclusive]
paths = ["Content/", "**/*.uasset", "**/*.umap", "Config/ProductionConfig.cpp"]

[shared]
paths = ["Source/", "Config/", "*.uproject", "docs/"]
```

A repository with World Partition and One File Per Actor stores each actor as its own file under `__ExternalActors__/` and
`__ExternalObjects__/`: thousands of small exclusive files. Locking a whole level at once, and an editor view of which actors a
level change touched, are not available yet. Until then, agree who works in which level, and keep an eye on the lock limit per user
([lock limit](concepts/pyn-toml.md#lock-limit), 5 by default; raise it in the policy for these projects).

## CI and build machines

A build machine only reads. Give it a token with read permission only ([access](concepts/access.md)) so a build can never take a
lock. A check that fails a build when it includes files locked by someone else is not available yet.

## Lock etiquette that scales

- Lock when you start, commit and push when you finish: a lock held overnight blocks the next person's morning.
- `pyn locks` shows who holds what. `pyn locks --watch` follows changes live.
- Lease length and the per-user lock limit are policy settings; an administrator can free a stuck lock with
  `pyn unlock --force --reason "..."`, which is recorded in the audit log.

# Content storage

How pyn stores file content (decision #194). Provisional where marked.

## Model

Content is stored as objects addressed by their git blob id: the hash of `blob <len>\0` followed by the uncompressed
bytes (decision #245). A pyn server repository is always SHA-256, so an id is 64 hex digits; the algorithm is part of
the id type (`ObjectId`, in `pyn-object`) and a SHA-1 id is never stored. Chunks are the exception: they keep the raw
SHA-256 of their bytes as their internal key, so the physical layout below is unchanged. Storage knows nothing about
whether a path is `shared` or `exclusive`, so a path that changes mode needs no data movement
([modes over time](pyn-toml.md)).

## Streaming

The `ObjectStore` trait works on streams: `put_stream` writes from a reader while hashing (the caller declares the length up front, so the blob header can be hashed
as the bytes stream; a body of any other length is refused as `length_mismatch`) and enforces a size limit,
`read` returns the object or a byte range as a stream, and `delete` removes an object. Whole-buffer `put` and `get` remain
as conveniences. The server streams uploads (`PUT /v1/repos/{owner}/{name}/objects`, up to 16 GiB per object, then
`413 object_too_large`; the `Content-Length` is the declared length, and a request without one is refused) and downloads, and `GET .../content` honours a single `Range: bytes=` header (`206`, or
`416` outside the content). The CLI streams uploads and `pyn get --output` downloads.

## Compression

The filesystem store compresses with zstd (level 3) when a probe of the object's first 64 KiB saves at least 5%;
objects under 512 bytes and already-compressed content stay raw. The per-blob marker is the file name: a compressed
object is `<id>.zst` beside where `<id>` would be, so existing raw objects keep working and content that happens to
look like a compressed frame is never misread. The key is always derived from the uncompressed bytes, `size` reports the
uncompressed length, and a damaged compressed object is an error, not a short read. The memory store keeps raw bytes.

## Chunking

Objects over 1 MiB are split by content-defined chunking (a gear-hash cut with normalised masks: 256 KiB minimum,
1 MiB average, 4 MiB maximum) into chunks stored as ordinary blobs, so each is compressed and deduplicated on its own.
A small manifest, stored under the whole content's blob id (`<id>.cm` on disk), lists the chunks' raw SHA-256 digests and lengths. Reads,
sizes and byte ranges work on the whole object; an insertion near the start of a large file changes only the chunks around
it. `ChunkedStore` wraps any store that can keep manifests, and the server uses it over the filesystem or memory store.
Chunks are not removed when an object is deleted: reclaiming them is garbage collection (#207).

## Chunked transfer

Over HTTP a client can send a large file as chunks (decision #76: plain HTTP, no new format):

1. Cut the file with the shared algorithm (`pyn-chunk`; the CLI and server use the same one) and take the raw SHA-256 of
   each chunk.
2. `POST /v1/repos/{owner}/{name}/objects/missing` with the digests (at most 10000 per call) returns the ones the server
   does not store.
3. `PUT .../objects/chunk` each missing chunk (at most 4 MiB); the response is its raw SHA-256 key. Whole objects go to
   `PUT .../objects` instead and get their blob id.
4. `POST .../objects/assemble` with the ordered chunk digests. The server sums the chunk lengths for the blob header,
   reads the chunks, computes the blob id of their concatenation (the client cannot claim one), stores the manifest, and
   returns that id to use in a commit. The CLI also hashes the file itself while cutting it and checks the two agree.

Repeating the sequence after a failure uploads only what is still missing. `GET .../content` sends `X-Pyn-Content` (the
content's blob id) and honours `Range`, so the CLI downloads through a `.pyn-part` file, continues a partial one with a `Range`
request, verifies the blob id, and only then moves the file into place. Files at or under 1 MiB use the single `PUT`. Parallel
chunk transfer is not done yet. An existence check answers for any repository on the server, so an id is only useful to
someone who already has the content.

## Migration from SHA-256 content hashes

Before decision #245 content was keyed by the plain SHA-256 of its bytes. At startup against PostgreSQL the server
re-stores every object a not yet migrated revision names under its blob id (a whole object is read through the old key and
written through the new one; chunks are untouched because they keep their raw SHA-256 key) and points the revisions at it.
Old keys stay until garbage collection. A revision whose object is missing is logged and keeps its old id. The step is
idempotent. Revision hashes keep covering the old content hash: see [tamper-evident history](integrity.md#existing-data).
The CLI converts its `.pyn/local_only/state/files.toml` when it next reads it.

## Trees and commits

Alongside the content, PostgreSQL keeps each commit's git tree objects (`trees`, keyed by repository and tree id, holding
git's tree encoding) and the commit records (`commits`). A commit's tree reuses its parent's unchanged subtrees. Blobs stay
in the object store; a tree names them by blob id and file mode. After the content ids are re-keyed the server migrates the
history into commits once (see [tamper-evident history](integrity.md#existing-data)); the memory store keeps the same
records in process.

## Planned (provisional)

- Mark-and-sweep garbage collection with a retention window.

See epic #196.

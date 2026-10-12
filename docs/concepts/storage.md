# Content storage

How pyn stores file content (decision #194). Provisional where marked.

## Model

Content is stored as objects addressed by the SHA-256 of their uncompressed bytes. Storage knows nothing about whether a
path is `shared` or `exclusive`, so a path that changes mode needs no data movement ([modes over time](pyn-toml.md)).

## Streaming

The `ObjectStore` trait works on streams: `put_stream` writes from a reader while hashing and enforces a size limit,
`read` returns the object or a byte range as a stream, and `delete` removes an object. Whole-buffer `put` and `get` remain
as conveniences. The server streams uploads (`PUT /v1/repos/{owner}/{name}/objects`, up to 16 GiB per object, then
`413 object_too_large`) and downloads, and `GET .../content` honours a single `Range: bytes=` header (`206`, or
`416` outside the content). The CLI streams uploads and `pyn get --output` downloads.

## Compression

The filesystem store compresses with zstd (level 3) when a probe of the object's first 64 KiB saves at least 5%;
objects under 512 bytes and already-compressed content stay raw. The per-blob marker is the file name: a compressed
object is `<hash>.zst` beside where `<hash>` would be, so existing raw objects keep working and content that happens to
look like a compressed frame is never misread. The key is always the hash of the uncompressed bytes, `size` reports the
uncompressed length, and a damaged compressed object is an error, not a short read. The memory store keeps raw bytes.

## Chunking

Objects over 1 MiB are split by content-defined chunking (a gear-hash cut with normalised masks: 256 KiB minimum,
1 MiB average, 4 MiB maximum) into chunks stored as ordinary blobs, so each is compressed and deduplicated on its own.
A small manifest, stored under the whole content's hash (`<hash>.cm` on disk), lists the chunk hashes and lengths. Reads,
sizes and byte ranges work on the whole object; an insertion near the start of a large file changes only the chunks around
it. `ChunkedStore` wraps any store that can keep manifests, and the server uses it over the filesystem or memory store.
Chunks are not removed when an object is deleted: reclaiming them is garbage collection (#207).

## Chunked transfer

Over HTTP a client can send a large file as chunks (decision #76: plain HTTP, no new format):

1. Cut the file with the shared algorithm (`pyn-chunk`; the CLI and server use the same one) and hash each chunk.
2. `POST /v1/repos/{owner}/{name}/objects/missing` with the hashes (at most 10000 per call) returns the ones the server does
   not store.
3. `PUT .../objects` each missing chunk; a chunk is an ordinary object (at most 4 MiB for assembly).
4. `POST .../objects/assemble` with the ordered chunk hashes. The server reads the chunks, computes the hash of their
   concatenation (the client cannot claim one), stores the manifest, and returns that hash to use in a commit.

Repeating the sequence after a failure uploads only what is still missing. `GET .../content` sends `X-Pyn-Content` (the
content hash) and honours `Range`, so the CLI downloads through a `.pyn-part` file, continues a partial one with a `Range`
request, verifies the hash, and only then moves the file into place. Files at or under 1 MiB use the single `PUT`. Parallel
chunk transfer is not done yet. An existence check answers for any repository on the server, so a hash is only useful to
someone who already has the content.

## Planned (provisional)

- Mark-and-sweep garbage collection with a retention window.

See epic #196.

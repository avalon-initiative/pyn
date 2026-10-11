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

## Planned (provisional)

- Content-defined chunking of objects over 1 MiB, with a manifest addressed by the whole-content hash.
- Chunk-granular upload with a missing-hashes check, so transfers resume and deduplicate.
- Mark-and-sweep garbage collection with a retention window.

See epic #196.

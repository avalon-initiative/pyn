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

## Planned (provisional)

- Zstd compression per blob with a one-byte codec marker.
- Content-defined chunking of objects over 1 MiB, with a manifest addressed by the whole-content hash.
- Chunk-granular upload with a missing-hashes check, so transfers resume and deduplicate.
- Mark-and-sweep garbage collection with a retention window.

See epic #196.

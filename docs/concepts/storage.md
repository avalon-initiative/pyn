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
2. `POST /v1/repos/{owner}/{name}/objects/missing` with the digests (at most 10000 per call) returns the ones the
   repository cannot reference yet (see [per-repository visibility](#per-repository-visibility)); a client uploads those.
3. `PUT .../objects/chunk` each missing chunk (at most 4 MiB); the response is its raw SHA-256 key. Whole objects go to
   `PUT .../objects` instead and get their blob id.
4. `POST .../objects/assemble` with the ordered chunk digests, each of which the repository must know (else
   `400 object_missing`); an empty list is `400 invalid_request` (an empty file is uploaded as a blob with `PUT .../objects`). The server sums the chunk lengths for the blob header,
   reads the chunks, computes the blob id of their concatenation (the client cannot claim one), stores the manifest, and
   returns that id to use in a commit. The CLI also hashes the file itself while cutting it and checks the two agree.

Repeating the sequence after a failure uploads only what is still missing. `GET .../content` sends `X-Pyn-Content` (the
content's blob id) and honours `Range`, so the CLI downloads through a `.pyn-part` file, continues a partial one with a `Range`
request, verifies the blob id, and only then moves the file into place. Files at or under 1 MiB use the single `PUT`. Parallel
chunk transfer is not done yet.

## Per-repository visibility

The object store is shared by every repository on the server and deduplicated by id. Each repository additionally keeps a
record of the object ids it may reference. An id enters the record when it is uploaded through that repository
(`PUT .../objects`, `PUT .../objects/chunk`, `POST .../objects/assemble`), and any content named by one of the
repository's revisions counts as recorded. Check-in, changesets, pushed trees, restore and merge resolutions accept a
content id only if the repository may reference it, and answer `400 object_missing` otherwise. `objects/missing` answers
relative to the repository, so an id another repository uploaded is reported missing; the client uploads it and the bytes
deduplicate in the shared store. The answer is the same whether or not the store holds the id. Reading a blob by id
(`GET .../objects/{id}`) still requires a revision of the repository to name it. Existing repositories are seeded from
their revisions by the migration; the chunks of content from before the record existed are uploaded again on first use.

An upload that no revision names yet is counted against the repository's unreferenced-upload ceiling (32 GiB by default; [`PYN_MAX_UNREFERENCED_UPLOAD_BYTES`](../configuration.md)). Past it,
an upload is refused with `409 storage_limit_reached`, naming the repository. The bytes stop counting when a revision
names them, and chunks stop counting once an assembled object holds them. Blobs an [LFS object](lfs.md) of the repository names
do not count either. These unreferenced records are also the input
a future garbage collection (#207) works from.

An upload that fails after storing bytes removes only the objects it newly created, and only those no repository has
recorded (this one included): another repository that uploaded identical bytes meanwhile keeps them. The check is
a separate step from the delete, so a record landing in the gap between the two can still lose its bytes; the window is
the time of one metadata query plus one delete, and a repository whose record points at missing bytes sees
`object_missing` and uploads again. If the check itself fails the object is left in place for garbage collection (#207).

While an upload streams in, the server writes it to a temp file named `.tmp-` and a random 128-bit suffix in the data
directory, created exclusively, and links it into place without replacing an object that exists. The temp file is
removed on every outcome, including a dropped connection. Temp files older than an hour, left by a process that died,
are removed at start and every 15 minutes.

## S3-compatible store

`pyn-s3` implements `ObjectStore` for any S3-compatible service (AWS S3, DigitalOcean Spaces, Cloudflare R2, MinIO), so
several server copies can share one bucket instead of a shared volume. It is provisional: the crate and its settings
exist, and choosing it at server start, health reporting and migrating an existing filesystem store are separate work
(decision #208, issues #1222 and #1223).

The client is written in house on `reqwest` (rustls) with AWS Signature Version 4: no AWS SDK and no new runtime
dependency. It sends only the headers it chooses: `Authorization`, `x-amz-date` and the plain full-payload
`x-amz-content-sha256`, never `aws-chunked`, `x-amz-checksum-*` or `Expect: 100-continue`, which several S3-compatible
services mishandle. Requests are virtual-host style (`https://<bucket>.<host>/<key>`) by default and path style
(`https://<host>/<bucket>/<key>`) by option; `http://` endpoints are allowed for local test servers. Redirects are not
followed. The secret key is never logged, and the settings' `Debug` output redacts it.

Operations used: PutObject, GetObject (with `Range`), HeadObject, DeleteObject, and for a spooled put over 5 GiB the
multipart calls (create, upload part, complete, abort). Nothing lists a bucket, so a key with no list permission is enough.
The store sits behind `ChunkedStore`, so it only sees whole objects up to 1 MiB, chunks up to 4 MiB and manifests.

### Keys

An object is stored at `<prefix><first two hex digits>/<rest>`, the layout of the filesystem store, and a manifest beside
it at the same key plus `.cm`. The optional prefix lets one bucket serve several installations. Compression is not
applied: the bytes stored are the bytes of the object.

### Why a put is hash-then-PUT

The key is the content hash and S3 can neither rename nor create an object only if its key matches its bytes, so a put
cannot stream into a temporary key and move it. The store reads the stream while hashing (the same hashing as the
filesystem store) into memory up to a threshold (8 MiB by default), and past it into a temporary spool file
(`.s3spool-` and a random 128-bit suffix in the configured directory). The declared length and the size limit are checked
before any request, so a refused put sends nothing. Then one HeadObject on the final key: when the object exists nothing
is uploaded and the put reports nothing created; otherwise one PutObject of the final key, from memory or from the spool
file. Content over 5 GiB (the single-PUT limit) goes up by multipart upload from the spool file at the final key, in
parts of 64 MiB or more, never more than 10000; a failed multipart upload is aborted. The spool file is removed on every
exit, including a dropped connection, and spool files older than an hour, left by a process that died, are removed at start.
Two servers putting the same new content at once both upload it and both report it created; the bytes are identical, so
the object is the same.

Reads use one GetObject with a `Range` header, clamped as the trait says; an object that does not exist is `None`.
`exists` and `size` are one HeadObject (`exists` also looks for the manifest when the object is absent), and `delete`
removes the object and its manifest. Chunks are not removed when an object is deleted, as with every store
([garbage collection](#chunking)).

### Failure behaviour

Every request has a connect timeout and an idle timeout, and calls without a large body a total timeout. Connection
errors, timeouts, `429` and `500`, `502`, `503` (including `SlowDown`) and `504` are retried with exponential backoff
and jitter, five attempts by default. A put of identical content, a HEAD, a GET, a DELETE, an upload part and an abort are
safe to repeat and are retried; creating and completing a multipart upload are not repeated blindly. Anything else is not
retried. An unreachable or refusing store is a `storage_error` that names the operation, the HTTP status and the S3 error
code, for example `S3 PutObject failed: HTTP 403 AccessDenied: Access Denied`. A failure while a read body is already
streaming surfaces to the reader and is not retried.

### Provider notes

- A bucket's access policy decides what a HEAD of a missing key answers: without `s3:ListBucket` AWS answers `403`
  rather than `404`, which this store reports as an error. Grant list permission on the bucket, or the store cannot tell
  absent from forbidden.
- DigitalOcean Spaces is the first hosted target. Spaces documents `x-amz-content-sha256` and ranged reads like S3; this
  client avoids the behaviours the documentation is silent on (`aws-chunked`, `x-amz-checksum-*`,
  `Expect: 100-continue`, conditional PUT, `UploadPartCopy`). Region for signing is the Space's region (`nyc3`, `ams3`);
  the endpoint is `https://<region>.digitaloceanspaces.com`. Not yet confirmed against a real Space: run the opt-in suite
  below with a throwaway Space.
- Parts must be at least 5 MiB except the last, and a multipart upload holds provider storage until completed or aborted;
  set a lifecycle rule that aborts incomplete uploads if a server can die mid-upload.

### Tests

`FsObjectStore`, `MemoryObjectStore` and `S3ObjectStore` all run one shared suite (`object_store_contract_tests!` in
`pyn-core`, behind its `contract` feature). The S3 store runs it every time against an in-process S3 server (`s3s` with
its filesystem backend, a dev dependency; no Docker), and against any real endpoint on request:

```
PYN_S3_TEST_ENDPOINT=https://nyc3.digitaloceanspaces.com PYN_S3_TEST_BUCKET=... \
PYN_S3_TEST_ACCESS_KEY_ID=... PYN_S3_TEST_SECRET_ACCESS_KEY=... \
cargo test -p pyn-s3 --features real-endpoint --test real_endpoint -- --ignored
```

`PYN_S3_TEST_REGION` (default `us-east-1`) and `PYN_S3_TEST_PATH_STYLE` (`true` for MinIO) are optional. Each test writes
under its own random `pyn-s3-test/` prefix and leaves its objects there. The signing code is checked against the
published Signature Version 4 examples for S3.

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

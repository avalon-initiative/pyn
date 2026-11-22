//! `pyn-server migrate-objects`: copies every stored object and chunk manifest from one object store to another,
//! so an install can move between the data directory and a bucket. It never deletes, and a re-run skips what is there.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt;
use pyn_core::{KeyHasher, Keying, ObjectId, ObjectStore, StoredKey, StoredKind};
use pyn_fs::FsObjectStore;
use tokio::io::AsyncReadExt;

use crate::host_admin::USAGE;

const PAGE: usize = 500;
const MAX_JOBS: usize = 64;
/// Failing keys printed in the summary; the rest are only counted.
const FAILURE_LIST_CAP: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Side {
    DataDir(PathBuf),
    /// The bucket named by the `PYN_S3_*` settings.
    S3,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub from: Side,
    pub to: Side,
    pub dry_run: bool,
    pub verify: bool,
    pub jobs: usize,
    pub force: bool,
}

impl Args {
    pub fn parse(args: &[String]) -> anyhow::Result<Self> {
        let (mut from, mut to) = (None, None);
        let (mut dry_run, mut verify, mut force, mut jobs) = (false, false, false, 4usize);
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let (name, inline) = match arg.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
                _ => (arg.as_str(), None),
            };
            let mut value = |what: &str| -> anyhow::Result<String> {
                inline
                    .clone()
                    .or_else(|| it.next().cloned())
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("{name} needs {what}\n{USAGE}"))
            };
            match name {
                "--from-data-dir" => from = Some(Side::DataDir(value("a directory")?.into())),
                "--to-data-dir" => to = Some(Side::DataDir(value("a directory")?.into())),
                "--jobs" => {
                    jobs = value("a number")?
                        .parse()
                        .ok()
                        .filter(|n| (1..=MAX_JOBS).contains(n))
                        .ok_or_else(|| {
                            anyhow::anyhow!("--jobs must be a number from 1 to {MAX_JOBS}")
                        })?;
                }
                "--from-s3" if inline.is_none() => from = Some(Side::S3),
                "--to-s3" if inline.is_none() => to = Some(Side::S3),
                "--dry-run" if inline.is_none() => dry_run = true,
                "--verify" if inline.is_none() => verify = true,
                "--force" if inline.is_none() => force = true,
                other => anyhow::bail!("unknown option {other}\n{USAGE}"),
            }
        }
        let from = from.ok_or_else(|| {
            anyhow::anyhow!("name the source with --from-data-dir PATH or --from-s3\n{USAGE}")
        })?;
        let to = to.ok_or_else(|| {
            anyhow::anyhow!("name the destination with --to-data-dir PATH or --to-s3\n{USAGE}")
        })?;
        anyhow::ensure!(
            !(from == Side::S3 && to == Side::S3),
            "source and destination are both the bucket named by PYN_S3_*; one side must be a data directory"
        );
        Ok(Self {
            from,
            to,
            dry_run,
            verify,
            jobs,
            force,
        })
    }
}

pub struct Options {
    pub dry_run: bool,
    pub verify: bool,
    pub jobs: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub key: String,
    pub reason: String,
}

/// What a run did; with `dry_run`, `copied` is what would be copied.
#[derive(Debug, Default)]
pub struct Report {
    pub copied: u64,
    pub copied_bytes: u64,
    pub skipped: u64,
    pub skipped_bytes: u64,
    pub verified: u64,
    pub failures: Vec<Failure>,
}

enum Outcome {
    Copied { bytes: u64, verified: bool },
    Skipped { bytes: u64 },
    Failed(String),
}

impl Report {
    fn add(&mut self, key: StoredKey, outcome: Outcome) {
        match outcome {
            Outcome::Copied { bytes, verified } => {
                self.copied += 1;
                self.copied_bytes += bytes;
                self.verified += u64::from(verified);
            }
            Outcome::Skipped { bytes } => {
                self.skipped += 1;
                self.skipped_bytes += bytes;
            }
            Outcome::Failed(reason) => self.failures.push(Failure {
                key: key.name(),
                reason,
            }),
        }
    }

    fn progress(&self) -> String {
        format!(
            "progress: {} copied ({} bytes), {} skipped, {} failed",
            self.copied,
            self.copied_bytes,
            self.skipped,
            self.failures.len()
        )
    }

    /// The summary for standard output.
    pub fn summary(&self, dry_run: bool, verify: bool) -> String {
        let mut text = format!(
            "{}: {} ({} bytes)\nskipped: {} ({} bytes)\n",
            if dry_run { "would copy" } else { "copied" },
            self.copied,
            self.copied_bytes,
            self.skipped,
            self.skipped_bytes
        );
        if verify {
            text.push_str(&format!("verified: {}\n", self.verified));
        }
        text.push_str(&format!("failed: {}\n", self.failures.len()));
        for f in self.failures.iter().take(FAILURE_LIST_CAP) {
            text.push_str(&format!("  {}: {}\n", f.key, f.reason));
        }
        if self.failures.len() > FAILURE_LIST_CAP {
            text.push_str(&format!(
                "  and {} more\n",
                self.failures.len() - FAILURE_LIST_CAP
            ));
        }
        text
    }
}

/// How an object's id derives from its bytes, found by hashing them: a whole object is a git blob id, a chunk the
/// raw SHA-256. `None` if the store no longer has it.
async fn keying_of(
    store: &dyn ObjectStore,
    id: &ObjectId,
) -> Result<Option<(Keying, u64)>, String> {
    let Some(found) = store.read(id, None).await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let total = found.total;
    let mut blob = KeyHasher::new(Keying::Blob { len: total });
    let mut raw = KeyHasher::new(Keying::Raw);
    let mut reader = found.reader;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        blob.update(&buf[..n]).map_err(|e| e.to_string())?;
        raw.update(&buf[..n]).map_err(|e| e.to_string())?;
    }
    if blob.finish().is_ok_and(|h| h == *id) {
        return Ok(Some((Keying::Blob { len: total }, total)));
    }
    if raw.finish().is_ok_and(|h| h == *id) {
        return Ok(Some((Keying::Raw, total)));
    }
    Err("the source bytes do not hash to their key".into())
}

/// Whether the object `store` holds under `id` rehashes to `id` under `keying`.
async fn holds_intact(
    store: &dyn ObjectStore,
    id: &ObjectId,
    keying: Keying,
) -> Result<bool, String> {
    let Some(found) = store.read(id, None).await.map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    let mut hasher = KeyHasher::new(keying);
    let mut reader = found.reader;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if hasher.update(&buf[..n]).is_err() {
            return Ok(false);
        }
    }
    Ok(hasher.finish().is_ok_and(|h| h == *id))
}

const GONE: &str = "missing from the source (removed during the run?)";
const DIFFERENT: &str = "the destination holds different content under this key; remove that object there and run again";

async fn copy_object(
    src: &dyn ObjectStore,
    dst: &dyn ObjectStore,
    id: ObjectId,
    opts: &Options,
) -> Result<Outcome, String> {
    let Some((keying, total)) = keying_of(src, &id).await? else {
        return Err(GONE.into());
    };
    match dst.size(&id).await.map_err(|e| e.to_string())? {
        Some(n) if n == total && holds_intact(dst, &id, keying).await? => {
            return Ok(Outcome::Skipped { bytes: total });
        }
        Some(_) => return Err(DIFFERENT.into()),
        None => {}
    }
    if opts.dry_run {
        return Ok(Outcome::Copied {
            bytes: total,
            verified: false,
        });
    }
    let found = src
        .read(&id, None)
        .await
        .map_err(|e| e.to_string())?
        .ok_or(GONE)?;
    let stored = dst
        .put_stream(found.reader, keying, None)
        .await
        .map_err(|e| e.to_string())?;
    if stored != id {
        return Err("the source changed while it was copied".into());
    }
    if opts.verify && !holds_intact(dst, &id, keying).await? {
        return Err("verification failed: the destination does not hold the same bytes".into());
    }
    Ok(Outcome::Copied {
        bytes: total,
        verified: opts.verify,
    })
}

async fn copy_manifest(
    src: &dyn ObjectStore,
    dst: &dyn ObjectStore,
    id: ObjectId,
    opts: &Options,
) -> Result<Outcome, String> {
    let bytes = src
        .get_manifest(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or(GONE)?;
    let len = bytes.len() as u64;
    let there = dst.get_manifest(&id).await.map_err(|e| e.to_string())?;
    if there.as_deref() == Some(bytes.as_slice()) {
        return Ok(Outcome::Skipped { bytes: len });
    }
    if !opts.dry_run {
        dst.put_manifest(&id, bytes.clone())
            .await
            .map_err(|e| e.to_string())?;
        if opts.verify {
            let back = dst.get_manifest(&id).await.map_err(|e| e.to_string())?;
            if back.as_deref() != Some(bytes.as_slice()) {
                return Err("verification failed: the destination manifest differs".into());
            }
        }
    }
    Ok(Outcome::Copied {
        bytes: len,
        verified: opts.verify && !opts.dry_run,
    })
}

async fn process(
    src: &Arc<dyn ObjectStore>,
    dst: &Arc<dyn ObjectStore>,
    keys: Vec<StoredKey>,
    opts: &Options,
    report: &mut Report,
) {
    let mut done = futures_util::stream::iter(keys)
        .map(|key| async move {
            let outcome = match key.kind {
                StoredKind::Object => copy_object(src.as_ref(), dst.as_ref(), key.id, opts).await,
                StoredKind::Manifest => {
                    copy_manifest(src.as_ref(), dst.as_ref(), key.id, opts).await
                }
            };
            (key, outcome.unwrap_or_else(Outcome::Failed))
        })
        .buffer_unordered(opts.jobs.max(1));
    while let Some((key, outcome)) = done.next().await {
        report.add(key, outcome);
    }
}

/// Copies every object, then every manifest, so a manifest never lands before the chunks it names.
/// A listing error stops the run; a failure on one key is recorded and the run goes on.
pub async fn migrate(
    src: Arc<dyn ObjectStore>,
    dst: Arc<dyn ObjectStore>,
    opts: &Options,
    progress: &(dyn Fn(&str) + Sync),
) -> anyhow::Result<Report> {
    let mut report = Report::default();
    let mut manifests = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = src
            .list_keys(after.as_deref(), PAGE)
            .await
            .map_err(|e| anyhow::anyhow!("listing the source: {e}"))?;
        let (objects, held): (Vec<_>, Vec<_>) = page
            .keys
            .into_iter()
            .partition(|k| k.kind == StoredKind::Object);
        manifests.extend(held);
        process(&src, &dst, objects, opts, &mut report).await;
        progress(&report.progress());
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    for batch in manifests.chunks(PAGE) {
        process(&src, &dst, batch.to_vec(), opts, &mut report).await;
        progress(&report.progress());
    }
    Ok(report)
}

/// `write` opens a destination for the copy (creates the directory, probes the bucket); otherwise nothing is changed.
async fn open_side(side: &Side, write: bool) -> anyhow::Result<(Arc<dyn ObjectStore>, String)> {
    match side {
        Side::DataDir(dir) => {
            let store = if write {
                FsObjectStore::open(dir).await
            } else {
                FsObjectStore::open_existing(dir).await
            }
            .map_err(|e| anyhow::anyhow!("data directory {}: {e}", dir.display()))?;
            Ok((Arc::new(store), format!("data directory {}", dir.display())))
        }
        Side::S3 => {
            let settings = crate::storage::s3_from_env(
                |name| std::env::var(name).ok(),
                |path| std::fs::read_to_string(path),
            )
            .map_err(anyhow::Error::msg)?;
            let bucket = settings.bucket.clone();
            let store: Arc<dyn ObjectStore> = if write {
                crate::storage::open_s3(settings).await?
            } else {
                Arc::new(
                    pyn_s3::S3ObjectStore::open(settings)
                        .await
                        .map_err(|e| anyhow::anyhow!("PYN_S3_*: {e}"))?,
                )
            };
            Ok((store, format!("bucket {bucket}")))
        }
    }
}

/// A directory's absolute form even before it exists, so two spellings of one place compare equal.
fn resolved(dir: &Path) -> PathBuf {
    dir.canonicalize()
        .or_else(|_| std::path::absolute(dir))
        .unwrap_or_else(|_| dir.to_path_buf())
}

/// Refuses two data directories that are the same or inside one another.
pub fn check_distinct(from: &Path, to: &Path) -> anyhow::Result<()> {
    let (a, b) = (resolved(from), resolved(to));
    anyhow::ensure!(
        !a.starts_with(&b) && !b.starts_with(&a),
        "source and destination are the same store (or one is inside the other); nothing to do"
    );
    Ok(())
}

/// Whether anything answers `/healthz` on `addr`, which is then taken to be a server writing to its store.
async fn server_answers(addr: &str) -> bool {
    let Ok(url) = crate::host_admin::healthz_url(addr) else {
        return false;
    };
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    client.get(url).send().await.is_ok()
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    if let (Side::DataDir(a), Side::DataDir(b)) = (&args.from, &args.to) {
        check_distinct(a, b)?;
    }
    let addr = std::env::var("PYN_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
    if !args.dry_run && !args.force && server_answers(&addr).await {
        anyhow::bail!(
            "a server answers on {addr}; stop it before migrating, since chunk and manifest writes would interleave with the copy (--force runs anyway)"
        );
    }
    let (src, from_name) = open_side(&args.from, false).await?;
    let absent = matches!(&args.to, Side::DataDir(d) if !d.exists());
    let (dst, to_name) = if args.dry_run && absent {
        let empty: Arc<dyn ObjectStore> = Arc::new(pyn_core::memory::MemoryObjectStore::new());
        (empty, "a new data directory".to_string())
    } else {
        open_side(&args.to, !args.dry_run).await?
    };
    eprintln!(
        "{} from {from_name} to {to_name}",
        if args.dry_run { "dry run" } else { "migrating" }
    );
    let opts = Options {
        dry_run: args.dry_run,
        verify: args.verify,
        jobs: args.jobs,
    };
    let report = migrate(src, dst, &opts, &|line| eprintln!("{line}")).await?;
    print!("{}", report.summary(args.dry_run, args.verify));
    anyhow::ensure!(
        report.failures.is_empty(),
        "{} object(s) failed; fix the cause and run again (a re-run skips what is already copied)",
        report.failures.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::ops::Range;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use pyn_core::memory::MemoryObjectStore;
    use pyn_core::{KeyPage, ObjectRead, ObjectReader};

    use super::*;

    /// A memory store whose reads can be made to lie: some ids read as absent, some as flipped bytes.
    #[derive(Default)]
    struct Tamper {
        inner: MemoryObjectStore,
        absent: Mutex<HashSet<ObjectId>>,
        flipped: Mutex<HashSet<ObjectId>>,
    }

    #[async_trait]
    impl ObjectStore for Tamper {
        async fn put_stream(
            &self,
            reader: ObjectReader,
            keying: Keying,
            max: Option<u64>,
        ) -> pyn_core::Result<ObjectId> {
            self.inner.put_stream(reader, keying, max).await
        }

        async fn read(
            &self,
            hash: &ObjectId,
            range: Option<Range<u64>>,
        ) -> pyn_core::Result<Option<ObjectRead>> {
            if self.absent.lock().unwrap().contains(hash) {
                return Ok(None);
            }
            let Some(mut found) = self.inner.read(hash, range).await? else {
                return Ok(None);
            };
            if self.flipped.lock().unwrap().contains(hash) {
                let mut bytes = Vec::new();
                found.reader.read_to_end(&mut bytes).await.unwrap();
                if let Some(b) = bytes.first_mut() {
                    *b ^= 0xff;
                }
                found.reader = Box::pin(std::io::Cursor::new(bytes));
            }
            Ok(Some(found))
        }

        async fn exists(&self, hash: &ObjectId) -> pyn_core::Result<bool> {
            self.inner.exists(hash).await
        }

        async fn size(&self, hash: &ObjectId) -> pyn_core::Result<Option<u64>> {
            self.inner.size(hash).await
        }

        async fn delete(&self, hash: &ObjectId) -> pyn_core::Result<bool> {
            self.inner.delete(hash).await
        }

        async fn put_manifest(&self, key: &ObjectId, bytes: Vec<u8>) -> pyn_core::Result<()> {
            self.inner.put_manifest(key, bytes).await
        }

        async fn get_manifest(&self, key: &ObjectId) -> pyn_core::Result<Option<Vec<u8>>> {
            self.inner.get_manifest(key).await
        }

        async fn list_keys(&self, after: Option<&str>, limit: usize) -> pyn_core::Result<KeyPage> {
            self.inner.list_keys(after, limit).await
        }
    }

    fn opts(jobs: usize) -> Options {
        Options {
            dry_run: false,
            verify: false,
            jobs,
        }
    }

    /// A source with `n` whole objects, a chunk and a manifest.
    async fn populated(n: usize) -> (Arc<Tamper>, Vec<ObjectId>) {
        let src = Arc::new(Tamper::default());
        let mut ids = Vec::new();
        for i in 0..n {
            ids.push(
                src.put(format!("object number {i}").into_bytes())
                    .await
                    .unwrap(),
            );
        }
        ids.push(src.put_chunk(b"a chunk".to_vec()).await.unwrap());
        src.put_manifest(&ids[0], b"manifest bytes".to_vec())
            .await
            .unwrap();
        (src, ids)
    }

    async fn run(src: &Arc<Tamper>, dst: &Arc<Tamper>, opts: &Options) -> Report {
        migrate(src.clone(), dst.clone(), opts, &|_| {})
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn everything_is_copied_then_a_second_run_copies_nothing() {
        let (src, ids) = populated(30).await;
        let dst = Arc::new(Tamper::default());
        let first = run(&src, &dst, &opts(4)).await;
        assert_eq!((first.copied, first.skipped), (32, 0));
        assert!(first.failures.is_empty());
        for id in &ids {
            assert_eq!(dst.size(id).await.unwrap(), src.size(id).await.unwrap());
        }
        assert_eq!(
            dst.get_manifest(&ids[0]).await.unwrap().unwrap(),
            b"manifest bytes"
        );
        let second = run(&src, &dst, &opts(4)).await;
        assert_eq!((second.copied, second.skipped), (0, 32));
        assert_eq!(second.skipped_bytes, first.copied_bytes);
    }

    #[tokio::test]
    async fn the_result_does_not_depend_on_the_number_of_jobs() {
        let (src, _) = populated(40).await;
        let (one, many) = (Arc::new(Tamper::default()), Arc::new(Tamper::default()));
        let a = run(&src, &one, &opts(1)).await;
        let b = run(&src, &many, &opts(16)).await;
        assert_eq!(
            (a.copied, a.copied_bytes, a.failures.len()),
            (b.copied, b.copied_bytes, b.failures.len())
        );
        let (ka, kb) = (
            one.list_keys(None, 1000).await.unwrap().keys,
            many.list_keys(None, 1000).await.unwrap().keys,
        );
        assert_eq!(ka.len(), kb.len());
    }

    #[tokio::test]
    async fn a_dry_run_counts_and_writes_nothing() {
        let (src, _) = populated(5).await;
        let dst = Arc::new(Tamper::default());
        let dry = Options {
            dry_run: true,
            ..opts(4)
        };
        let report = run(&src, &dst, &dry).await;
        assert_eq!(report.copied, 7);
        assert!(report.copied_bytes > 0);
        assert!(dst.list_keys(None, 1000).await.unwrap().keys.is_empty());
        assert!(report.summary(true, false).starts_with("would copy: 7"));
    }

    #[tokio::test]
    async fn the_source_is_left_untouched() {
        let (src, _) = populated(5).await;
        let before = src.list_keys(None, 1000).await.unwrap().keys.len();
        run(&src, &Arc::new(Tamper::default()), &opts(2)).await;
        assert_eq!(src.list_keys(None, 1000).await.unwrap().keys.len(), before);
    }

    #[tokio::test]
    async fn verify_catches_a_destination_that_reads_back_wrong() {
        let (src, ids) = populated(5).await;
        let dst = Arc::new(Tamper::default());
        dst.flipped.lock().unwrap().insert(ids[2]);
        let verify = Options {
            verify: true,
            ..opts(2)
        };
        let report = run(&src, &dst, &verify).await;
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].key, ids[2].to_hex());
        assert!(report.failures[0].reason.contains("verification"));
        assert_eq!(report.verified, report.copied);
    }

    #[tokio::test]
    async fn a_corrupted_destination_object_is_reported_on_the_next_run() {
        let (src, ids) = populated(5).await;
        let dst = Arc::new(Tamper::default());
        run(&src, &dst, &opts(2)).await;
        dst.flipped.lock().unwrap().insert(ids[1]);
        let report = run(&src, &dst, &opts(2)).await;
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].key, ids[1].to_hex());
        assert!(report.failures[0].reason.contains("different content"));
        assert_eq!(report.skipped, 6);
    }

    #[tokio::test]
    async fn a_source_object_that_vanishes_is_reported_and_the_rest_copied() {
        let (src, ids) = populated(5).await;
        src.absent.lock().unwrap().insert(ids[3]);
        let dst = Arc::new(Tamper::default());
        let report = run(&src, &dst, &opts(3)).await;
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].key, ids[3].to_hex());
        assert!(
            report.failures[0]
                .reason
                .contains("missing from the source")
        );
        assert_eq!(report.copied, 6);
        assert!(dst.size(&ids[3]).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_source_object_that_does_not_hash_to_its_key_is_not_copied() {
        let (src, ids) = populated(3).await;
        src.flipped.lock().unwrap().insert(ids[0]);
        let dst = Arc::new(Tamper::default());
        let report = run(&src, &dst, &opts(2)).await;
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].reason.contains("do not hash"));
        assert!(dst.size(&ids[0]).await.unwrap().is_none());
    }

    #[test]
    fn the_failure_list_is_capped() {
        let report = Report {
            failures: (0..45)
                .map(|n| Failure {
                    key: format!("key{n}"),
                    reason: "bad".into(),
                })
                .collect(),
            ..Report::default()
        };
        let text = report.summary(false, false);
        assert!(text.contains("failed: 45"));
        assert!(text.contains("key19") && !text.contains("key20"));
        assert!(text.contains("and 25 more"));
    }

    #[tokio::test]
    async fn a_store_that_cannot_list_is_an_error() {
        struct Blind(MemoryObjectStore);
        #[async_trait]
        impl ObjectStore for Blind {
            async fn put_stream(
                &self,
                r: ObjectReader,
                k: Keying,
                m: Option<u64>,
            ) -> pyn_core::Result<ObjectId> {
                self.0.put_stream(r, k, m).await
            }
            async fn read(
                &self,
                h: &ObjectId,
                r: Option<Range<u64>>,
            ) -> pyn_core::Result<Option<ObjectRead>> {
                self.0.read(h, r).await
            }
            async fn exists(&self, h: &ObjectId) -> pyn_core::Result<bool> {
                self.0.exists(h).await
            }
            async fn size(&self, h: &ObjectId) -> pyn_core::Result<Option<u64>> {
                self.0.size(h).await
            }
            async fn delete(&self, h: &ObjectId) -> pyn_core::Result<bool> {
                self.0.delete(h).await
            }
        }
        let blind: Arc<dyn ObjectStore> = Arc::new(Blind(MemoryObjectStore::new()));
        let err = migrate(blind, Arc::new(MemoryObjectStore::new()), &opts(1), &|_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot list"), "{err}");
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_flags_name_both_sides_and_default_to_four_jobs() {
        let a = Args::parse(&args(&["--from-data-dir", "/d", "--to-s3"])).unwrap();
        assert_eq!(a.from, Side::DataDir("/d".into()));
        assert_eq!(
            (a.to, a.jobs, a.dry_run, a.verify, a.force),
            (Side::S3, 4, false, false, false)
        );
        let a = Args::parse(&args(&[
            "--from-s3",
            "--to-data-dir=/e",
            "--jobs",
            "9",
            "--dry-run",
            "--verify",
            "--force",
        ]))
        .unwrap();
        assert_eq!(a.to, Side::DataDir("/e".into()));
        assert_eq!(
            (a.jobs, a.dry_run, a.verify, a.force),
            (9, true, true, true)
        );
    }

    #[test]
    fn bad_flags_are_refused() {
        for bad in [
            &[][..],
            &["--from-s3"],
            &["--to-s3"],
            &["--from-s3", "--to-s3"],
            &["--from-data-dir"],
            &["--from-data-dir", "/d", "--to-s3", "--jobs", "0"],
            &["--from-data-dir", "/d", "--to-s3", "--jobs", "65"],
            &["--from-data-dir", "/d", "--to-s3", "--jobs", "x"],
            &["--from-data-dir", "/d", "--to-s3", "--delete"],
            &["--from-data-dir", "/d", "--to-s3", "--dry-run=yes"],
        ] {
            assert!(Args::parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_same_directory_or_a_nested_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        assert!(check_distinct(dir.path(), dir.path()).is_err());
        assert!(check_distinct(dir.path(), &dir.path().join("./inner/..")).is_err());
        assert!(check_distinct(dir.path(), &inner).is_err());
        assert!(check_distinct(&inner, dir.path()).is_err());
        assert!(check_distinct(&inner, &dir.path().join("new")).is_ok());
    }
}

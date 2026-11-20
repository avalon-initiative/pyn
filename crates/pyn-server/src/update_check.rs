//! The opt-in lookup of newer pyn releases (`PYN_UPDATE_CHECK=on`). A background task refreshes a cached answer; requests
//! only read it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pyn_core::Clock;
use pyn_proto as api;
use semver::Version;
use serde::Deserialize;

/// How long a successful answer is kept.
pub const TTL: chrono::Duration = chrono::Duration::hours(24);
/// How long to wait after a failed lookup before trying again.
pub const RETRY_AFTER: chrono::Duration = chrono::Duration::hours(1);

const RELEASES_URL: &str =
    "https://api.github.com/repos/avalon-initiative/pyn/releases?per_page=30";
const MAX_BODY: usize = 2 << 20;

/// A published release as the source reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    pub url: String,
}

/// Where release information comes from.
#[async_trait]
pub trait ReleaseSource: Send + Sync {
    /// The published (non-draft) releases, in any order.
    async fn releases(&self) -> Result<Vec<Release>, String>;
}

/// The GitHub Releases API of the pyn repository. The request carries only a generic user agent.
pub struct GithubReleases {
    http: reqwest::Client,
}

impl GithubReleases {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent("pyn-server")
                .timeout(Duration::from_secs(15))
                .build()?,
        })
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

#[async_trait]
impl ReleaseSource for GithubReleases {
    async fn releases(&self) -> Result<Vec<Release>, String> {
        let reply = self
            .http
            .get(RELEASES_URL)
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        let body = reply.bytes().await.map_err(|e| e.to_string())?;
        if body.len() > MAX_BODY {
            return Err("the release list is unexpectedly large".into());
        }
        let list: Vec<GithubRelease> = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
        Ok(list
            .into_iter()
            .filter(|r| !r.draft)
            .map(|r| Release {
                tag: r.tag_name,
                prerelease: r.prerelease,
                url: r.html_url,
            })
            .collect())
    }
}

fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// The newest release above `current` on its channel: pre-releases count only when `current` is one itself.
/// Tags that are not versions are ignored.
pub fn newer_release(current: &str, releases: &[Release]) -> Option<Release> {
    let current = Version::parse(current).ok()?;
    let with_pre = !current.pre.is_empty();
    releases
        .iter()
        .filter_map(|r| Some((parse_tag(&r.tag)?, r)))
        .filter(|(v, _)| with_pre || v.pre.is_empty())
        .filter(|(v, _)| *v > current)
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, r)| r.clone())
}

#[derive(Default)]
struct Cache {
    attempted_at: Option<DateTime<Utc>>,
    failed: bool,
    checked_at: Option<DateTime<Utc>>,
    latest: Option<Release>,
}

pub struct UpdateChecker {
    source: Arc<dyn ReleaseSource>,
    clock: Arc<dyn Clock>,
    current: String,
    cache: Mutex<Cache>,
    refreshing: tokio::sync::Mutex<()>,
}

impl UpdateChecker {
    pub fn new(source: Arc<dyn ReleaseSource>, clock: Arc<dyn Clock>, current: &str) -> Self {
        Self {
            source,
            clock,
            current: current.to_string(),
            cache: Mutex::new(Cache::default()),
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    /// The cached answer; never touches the network.
    pub fn status(&self) -> api::UpdateStatus {
        let cache = self.cache.lock().unwrap();
        let latest = cache.latest.as_ref();
        api::UpdateStatus {
            enabled: true,
            current: self.current.clone(),
            latest: latest.map(|r| r.tag.strip_prefix('v').unwrap_or(&r.tag).to_string()),
            prerelease: latest
                .map(|r| r.prerelease || parse_tag(&r.tag).is_some_and(|v| !v.pre.is_empty())),
            url: latest.map(|r| r.url.clone()),
            checked_at: cache.checked_at,
        }
    }

    /// Looks up releases unless the cache is fresh: a day after a success, an hour after a failure.
    /// Returns whether the source was asked.
    pub async fn refresh_if_due(&self) -> bool {
        let _one_at_a_time = self.refreshing.lock().await;
        let now = self.clock.now();
        {
            let cache = self.cache.lock().unwrap();
            let wait = if cache.failed { RETRY_AFTER } else { TTL };
            if cache.attempted_at.is_some_and(|at| now < at + wait) {
                return false;
            }
        }
        let found = self.source.releases().await;
        let mut cache = self.cache.lock().unwrap();
        cache.attempted_at = Some(now);
        match found {
            Ok(releases) => {
                cache.failed = false;
                cache.checked_at = Some(now);
                cache.latest = newer_release(&self.current, &releases);
            }
            Err(why) => {
                cache.failed = true;
                tracing::debug!(error = %why, "update check failed");
            }
        }
        true
    }

    /// Checks now, then whenever due, polling every `every`.
    pub fn spawn(self: Arc<Self>, every: Duration) {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                self.refresh_if_due().await;
            }
        });
    }
}

/// The answer when the check is off.
pub fn disabled(current: &str) -> api::UpdateStatus {
    api::UpdateStatus {
        enabled: false,
        current: current.to_string(),
        latest: None,
        prerelease: None,
        url: None,
        checked_at: None,
    }
}

/// A source for tests: a fixed answer, counting how often it is asked.
pub struct FakeReleaseSource {
    answer: Mutex<Result<Vec<Release>, String>>,
    calls: std::sync::atomic::AtomicUsize,
}

impl FakeReleaseSource {
    pub fn new(answer: Result<Vec<Release>, String>) -> Self {
        Self {
            answer: Mutex::new(answer),
            calls: Default::default(),
        }
    }

    pub fn set(&self, answer: Result<Vec<Release>, String>) {
        *self.answer.lock().unwrap() = answer;
    }

    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl ReleaseSource for FakeReleaseSource {
    async fn releases(&self) -> Result<Vec<Release>, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.answer.lock().unwrap().clone()
    }
}

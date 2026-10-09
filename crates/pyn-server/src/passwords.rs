use std::sync::Arc;

use async_trait::async_trait;
use pyn_core::{PasswordWorker, PynError, account};
use tokio::sync::Semaphore;

/// Hashes on tokio's blocking pool, at most `limit` at a time, so sign-in traffic cannot stall the request
/// threads or exhaust memory (each argon2id hash uses 19 MiB).
pub struct PooledPasswords {
    permits: Arc<Semaphore>,
}

impl PooledPasswords {
    pub fn new(limit: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit.max(1))),
        }
    }

    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, PynError> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| PynError::Storage("password hashing is shut down".into()))?;
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|e| PynError::Storage(format!("password hashing failed: {e}")))
    }
}

#[async_trait]
impl PasswordWorker for PooledPasswords {
    async fn hash(&self, password: &str) -> pyn_core::Result<String> {
        let password = password.to_string();
        self.run(move || account::hash_password(&password)).await?
    }

    async fn verify(&self, password: &str, hash: &str) -> bool {
        let (password, hash) = (password.to_string(), hash.to_string());
        self.run(move || account::verify_password(&password, &hash))
            .await
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn at_most_the_limit_run_at_once() {
        let pool = Arc::new(PooledPasswords::new(2));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let (pool, running, peak) = (pool.clone(), running.clone(), peak.clone());
                tokio::spawn(async move {
                    pool.run(move || {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(20));
                        running.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await
                    .unwrap();
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn work_leaves_the_request_thread_and_the_runtime_stays_responsive() {
        let pool = PooledPasswords::new(1);
        let caller = std::thread::current().id();
        let slow = pool.run(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::thread::current().id()
        });
        let ticks = async {
            for _ in 0..3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let (worker, ()) = tokio::join!(slow, ticks);
        assert_ne!(worker.unwrap(), caller);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hashes_verify_through_the_pool() {
        let pool = PooledPasswords::new(2);
        let hash = pool.hash("correct horse battery").await.unwrap();
        assert!(pool.verify("correct horse battery", &hash).await);
        assert!(!pool.verify("wrong horse battery", &hash).await);
        assert!(!pool.verify("anything", "not a hash").await);
    }
}

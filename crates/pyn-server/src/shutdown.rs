//! Graceful shutdown: a shared draining flag, the in-flight request count and the loop that stops the server.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use tokio::sync::{mpsc, watch};

/// Whether the server is shutting down, whether its listener has closed, and how many requests are in flight.
/// Clones share state.
#[derive(Clone)]
pub struct Drain {
    flag: Arc<watch::Sender<bool>>,
    closed: Arc<watch::Sender<bool>>,
    in_flight: Arc<AtomicUsize>,
}

impl Default for Drain {
    fn default() -> Self {
        Self {
            flag: Arc::new(watch::channel(false).0),
            closed: Arc::new(watch::channel(false).0),
            in_flight: Arc::default(),
        }
    }
}

impl Drain {
    pub fn start(&self) {
        self.flag.send_replace(true);
    }

    pub fn is_draining(&self) -> bool {
        *self.flag.borrow()
    }

    /// Resolves once draining has started; at once if it already has.
    pub async fn started(&self) {
        let mut rx = self.flag.subscribe();
        let _ = rx.wait_for(|draining| *draining).await;
    }

    /// Tells the server to stop accepting connections; draining alone keeps it accepting.
    pub fn close(&self) {
        self.closed.send_replace(true);
    }

    /// Resolves once the listener should close; at once if it already should.
    pub async fn closed(&self) {
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(|closed| *closed).await;
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Relaxed)
    }

    /// Waits for the next tick of a background loop; false once draining has started, so the loop ends.
    /// A pass already running finishes: only the wait is interrupted.
    pub async fn tick(&self, interval: &mut tokio::time::Interval) -> bool {
        if self.is_draining() {
            return false;
        }
        tokio::select! {
            _ = interval.tick() => !self.is_draining(),
            () = self.started() => false,
        }
    }

    /// Sleeps for `pause`; false when draining started first.
    pub async fn sleep(&self, pause: Duration) -> bool {
        tokio::select! {
            () = tokio::time::sleep(pause) => !self.is_draining(),
            () = self.started() => false,
        }
    }
}

struct InFlight(Arc<AtomicUsize>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Counts the request until its response is done, so a forced exit can say how many it cut off.
pub(crate) async fn track(State(drain): State<Drain>, request: Request, next: Next) -> Response {
    drain.in_flight.fetch_add(1, Ordering::Relaxed);
    let _guard = InFlight(drain.in_flight.clone());
    next.run(request).await
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The server stopped on its own, with every request finished.
    Clean,
    /// The grace period ran out or a second signal arrived with `cut_off` (at least 1) requests still running.
    Forced { cut_off: usize },
}

/// Runs `server` until the first message on `signals`, starts draining, keeps serving for `delay` (a second signal
/// ends it early), then closes the listener and waits up to `grace`, counted from that moment, for `server` (which
/// must stop accepting on `drain.closed()`) to finish. A second signal during the grace, or its end, forces the exit.
pub async fn run<F>(
    server: F,
    drain: &Drain,
    delay: Duration,
    grace: Duration,
    mut signals: mpsc::UnboundedReceiver<()>,
) -> std::io::Result<Outcome>
where
    F: Future<Output = std::io::Result<()>>,
{
    tokio::pin!(server);
    tokio::select! {
        done = &mut server => {
            done?;
            return Ok(Outcome::Clean);
        }
        _ = signals.recv() => {}
    }
    drain.start();
    if !delay.is_zero() {
        tracing::info!(
            delay_secs = delay.as_secs(),
            "shutting down: not ready, still accepting connections until the drain delay ends"
        );
        tokio::select! {
            biased;
            done = &mut server => {
                done?;
                return Ok(Outcome::Clean);
            }
            () = tokio::time::sleep(delay) => {}
            _ = signals.recv() => tracing::info!("a second signal arrived: ending the drain delay"),
        }
    }
    tracing::info!(
        in_flight = drain.in_flight(),
        grace_secs = grace.as_secs(),
        "shutting down: not accepting new connections, waiting for requests in flight"
    );
    drain.close();
    tokio::select! {
        biased;
        done = &mut server => {
            done?;
            Ok(Outcome::Clean)
        }
        () = tokio::time::sleep(grace) => Ok(forced(drain, "the grace period ran out")),
        _ = signals.recv() => Ok(forced(drain, "a second signal arrived")),
    }
}

fn forced(drain: &Drain, why: &str) -> Outcome {
    let cut_off = drain.in_flight();
    if cut_off == 0 {
        tracing::info!("exiting now: {why}; no request was running");
        return Outcome::Clean;
    }
    tracing::error!(
        cut_off,
        "exiting now: {why}; {cut_off} request(s) were still running and are cut off"
    );
    Outcome::Forced { cut_off }
}

/// Forwards SIGTERM and Ctrl-C (SIGINT) to a channel, one message per signal. Installed on the calling runtime.
pub fn watch_signals() -> std::io::Result<mpsc::UnboundedReceiver<()>> {
    let (tx, rx) = mpsc::unbounded_channel();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
                if tx.send(()).is_err() {
                    break;
                }
            }
        });
    }
    #[cfg(not(unix))]
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            if tx.send(()).is_err() {
                break;
            }
        }
    });
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_flag_starts_clear_and_stays_set() {
        let drain = Drain::default();
        assert!(!drain.is_draining());
        let other = drain.clone();
        drain.start();
        assert!(other.is_draining());
        other.started().await;
    }

    #[tokio::test]
    async fn a_loop_tick_ends_when_draining_starts() {
        let drain = Drain::default();
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        assert!(drain.tick(&mut tick).await);
        let waiting = tokio::spawn({
            let drain = drain.clone();
            async move {
                drain
                    .tick(&mut tokio::time::interval(Duration::from_secs(3600)))
                    .await
            }
        });
        tokio::task::yield_now().await;
        drain.start();
        assert!(!waiting.await.unwrap());
        assert!(!drain.tick(&mut tick).await);
        assert!(!drain.sleep(Duration::from_secs(3600)).await);
    }

    #[tokio::test]
    async fn requests_in_flight_are_counted_until_dropped() {
        let drain = Drain::default();
        drain.in_flight.fetch_add(1, Ordering::Relaxed);
        let guard = InFlight(drain.in_flight.clone());
        assert_eq!(drain.in_flight(), 1);
        drop(guard);
        assert_eq!(drain.in_flight(), 0);
    }

    fn server(drain: &Drain, finish_after: Duration) -> impl Future<Output = std::io::Result<()>> {
        let drain = drain.clone();
        async move {
            drain.closed().await;
            tokio::time::sleep(finish_after).await;
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_finishes_inside_the_grace_is_clean() {
        let drain = Drain::default();
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(()).unwrap();
        let out = run(
            server(&drain, Duration::from_secs(5)),
            &drain,
            Duration::ZERO,
            Duration::from_secs(30),
            rx,
        )
        .await
        .unwrap();
        assert_eq!(out, Outcome::Clean);
        assert!(drain.is_draining());
    }

    #[tokio::test(start_paused = true)]
    async fn the_end_of_the_grace_forces_the_exit_and_counts_the_cut_off() {
        let drain = Drain::default();
        drain.in_flight.fetch_add(2, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(()).unwrap();
        let out = run(
            server(&drain, Duration::from_secs(60)),
            &drain,
            Duration::ZERO,
            Duration::from_secs(30),
            rx,
        )
        .await
        .unwrap();
        assert_eq!(out, Outcome::Forced { cut_off: 2 });
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_signal_forces_the_exit_at_once() {
        let drain = Drain::default();
        drain.in_flight.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(()).unwrap();
        tx.send(()).unwrap();
        let out = run(
            server(&drain, Duration::from_secs(60)),
            &drain,
            Duration::ZERO,
            Duration::from_secs(30),
            rx,
        )
        .await
        .unwrap();
        assert!(matches!(out, Outcome::Forced { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn no_grace_still_lets_an_idle_server_exit_clean() {
        let drain = Drain::default();
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(()).unwrap();
        let out = run(
            server(&drain, Duration::ZERO),
            &drain,
            Duration::ZERO,
            Duration::ZERO,
            rx,
        )
        .await
        .unwrap();
        assert_eq!(out, Outcome::Clean);
    }

    #[tokio::test(start_paused = true)]
    async fn the_end_of_the_grace_with_nothing_running_is_clean() {
        let drain = Drain::default();
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(()).unwrap();
        let out = run(
            server(&drain, Duration::from_secs(60)),
            &drain,
            Duration::ZERO,
            Duration::from_secs(30),
            rx,
        )
        .await
        .unwrap();
        assert_eq!(out, Outcome::Clean);
    }

    #[tokio::test(start_paused = true)]
    async fn the_listener_stays_open_for_the_delay_and_the_grace_counts_from_its_close() {
        let drain = Drain::default();
        drain.in_flight.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(()).unwrap();
        let started = tokio::time::Instant::now();
        let watcher = tokio::spawn({
            let drain = drain.clone();
            async move {
                drain.started().await;
                let draining_at = started.elapsed();
                drain.closed().await;
                (draining_at, started.elapsed())
            }
        });
        let out = run(
            server(&drain, Duration::from_secs(600)),
            &drain,
            Duration::from_secs(20),
            Duration::from_secs(30),
            rx,
        )
        .await
        .unwrap();
        assert_eq!(out, Outcome::Forced { cut_off: 1 });
        let (draining_at, closed_at) = watcher.await.unwrap();
        assert_eq!(draining_at, Duration::ZERO);
        assert_eq!(closed_at, Duration::from_secs(20));
        assert_eq!(started.elapsed(), Duration::from_secs(50));
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_signal_during_the_delay_skips_it_and_the_grace_still_applies() {
        let drain = Drain::default();
        drain.in_flight.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(()).unwrap();
        let started = tokio::time::Instant::now();
        let second = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            tx.send(()).unwrap();
            tx
        });
        let out = run(
            server(&drain, Duration::from_secs(600)),
            &drain,
            Duration::from_secs(20),
            Duration::from_secs(30),
            rx,
        )
        .await
        .unwrap();
        let _tx = second.await.unwrap();
        assert_eq!(out, Outcome::Forced { cut_off: 1 });
        assert_eq!(started.elapsed(), Duration::from_secs(35));
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_ends_during_the_delay_is_clean() {
        let drain = Drain::default();
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(()).unwrap();
        let out = run(
            async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            },
            &drain,
            Duration::from_secs(20),
            Duration::from_secs(30),
            rx,
        )
        .await
        .unwrap();
        assert_eq!(out, Outcome::Clean);
        assert!(drain.is_draining());
    }
}

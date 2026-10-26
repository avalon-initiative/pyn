//! Webhook store behaviour: fan-out, exclusive claiming, leases, backoff, retention, secrets and redelivery.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::Webhooks;
use crate::UserId;
use crate::webhook::{
    DeliveryQuery, DeliveryStatus, FixedJitter, MAX_ATTEMPTS, NewSecret, NewSubscription,
    SubscriptionUpdate, retention, retry_delay,
};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap()
}

fn sub(name: &str, types: &[&str], enabled: bool) -> NewSubscription {
    NewSubscription {
        name: name.to_string(),
        url: format!("https://example.test/{name}"),
        event_types: types.iter().map(|t| t.to_string()).collect(),
        enabled,
        created_by: UserId::new("amy"),
    }
}

fn secret(byte: u8) -> NewSecret {
    NewSecret {
        secret: vec![byte; 32],
        encrypted: false,
        key_id: None,
    }
}

const PAYLOAD: &str = r#"{"k":1}"#;

pub async fn subscriptions_are_created_listed_updated_and_deleted(store: Webhooks) {
    let a = store
        .create_subscription(sub("a", &["repo.created"], true), t0())
        .await
        .unwrap();
    let b = store
        .create_subscription(sub("b", &[], true), t0())
        .await
        .unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(store.get_subscription(a.id).await.unwrap().unwrap(), a);
    assert_eq!(store.list_subscriptions().await.unwrap().len(), 2);

    let later = t0() + Duration::hours(1);
    let upd = SubscriptionUpdate {
        url: Some("https://example.test/moved".into()),
        enabled: Some(false),
        ..Default::default()
    };
    let a2 = store
        .update_subscription(a.id, upd, later)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a2.url, "https://example.test/moved");
    assert!(!a2.enabled);
    assert_eq!(
        (a2.name.as_str(), a2.event_types.clone()),
        ("a", a.event_types)
    );
    assert_eq!((a2.created_at, a2.updated_at), (t0(), later));
    assert!(
        store
            .update_subscription(999_999, SubscriptionUpdate::default(), later)
            .await
            .unwrap()
            .is_none()
    );

    store.add_secret(a.id, secret(1), t0()).await.unwrap();
    store.append_event("x", PAYLOAD, t0()).await.unwrap();
    assert!(store.delete_subscription(b.id).await.unwrap());
    assert!(!store.delete_subscription(b.id).await.unwrap());
    assert!(store.get_subscription(b.id).await.unwrap().is_none());
    assert!(
        store
            .list_deliveries(
                b.id,
                &DeliveryQuery {
                    limit: 10,
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.list_subscriptions().await.unwrap().len(), 1);
}

pub async fn fan_out_follows_the_event_type_filter_and_the_enabled_flag(store: Webhooks) {
    let all = store
        .create_subscription(sub("all", &[], true), t0())
        .await
        .unwrap();
    let repos = store
        .create_subscription(sub("repos", &["repo.created", "repo.deleted"], true), t0())
        .await
        .unwrap();
    let orgs = store
        .create_subscription(sub("orgs", &["org.created"], true), t0())
        .await
        .unwrap();
    let off = store
        .create_subscription(sub("off", &[], false), t0())
        .await
        .unwrap();

    let q = DeliveryQuery {
        limit: 10,
        ..Default::default()
    };
    let event = store
        .append_event("repo.created", PAYLOAD, t0())
        .await
        .unwrap();
    assert_eq!(event.event_type, "repo.created");
    assert_eq!(event.created_at, t0());
    assert!(event.payload.contains("\"k\""));

    for (s, want) in [(&all, 1), (&repos, 1), (&orgs, 0), (&off, 0)] {
        let rows = store.list_deliveries(s.id, &q).await.unwrap();
        assert_eq!(rows.len(), want, "{}", s.name);
        for d in rows {
            assert_eq!(d.event_id, event.id);
            assert_eq!(d.status, DeliveryStatus::Pending);
            assert_eq!(d.attempt_count, 0);
            assert_eq!(d.next_attempt_at, t0());
            assert!(d.claimed_until.is_none());
        }
    }

    // Enabling later does not backfill earlier events.
    let upd = SubscriptionUpdate {
        enabled: Some(true),
        ..Default::default()
    };
    store.update_subscription(off.id, upd, t0()).await.unwrap();
    assert!(store.list_deliveries(off.id, &q).await.unwrap().is_empty());
    store
        .append_event("org.created", PAYLOAD, t0())
        .await
        .unwrap();
    assert_eq!(store.list_deliveries(off.id, &q).await.unwrap().len(), 1);
    assert_eq!(store.list_deliveries(orgs.id, &q).await.unwrap().len(), 1);
    assert_eq!(store.list_deliveries(repos.id, &q).await.unwrap().len(), 1);
}

pub async fn a_delivery_is_claimed_only_when_due_and_only_by_one_claimer(store: Webhooks) {
    let s = store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    for _ in 0..3 {
        store.append_event("e", PAYLOAD, t0()).await.unwrap();
    }
    let lease = Duration::minutes(5);

    assert!(
        store
            .claim_due(10, lease, t0() - Duration::seconds(1))
            .await
            .unwrap()
            .is_empty(),
        "not due before next_attempt_at"
    );
    let first = store.claim_due(2, lease, t0()).await.unwrap();
    assert_eq!(first.len(), 2, "limit honoured");
    for c in &first {
        assert_eq!(c.subscription.id, s.id);
        assert!(c.event.payload.contains("\"k\""));
        assert_eq!(c.delivery.claimed_until, Some(t0() + lease));
    }
    let second = store.claim_due(10, lease, t0()).await.unwrap();
    assert_eq!(second.len(), 1, "claimed rows are skipped");
    assert!(store.claim_due(10, lease, t0()).await.unwrap().is_empty());
}

pub async fn racing_claimers_never_receive_the_same_delivery(store: Webhooks) {
    store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    store
        .create_subscription(sub("t", &[], true), t0())
        .await
        .unwrap();
    const EVENTS: usize = 30;
    for _ in 0..EVENTS {
        store.append_event("e", PAYLOAD, t0()).await.unwrap();
    }
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                let mut ids = Vec::new();
                loop {
                    let got = store
                        .claim_due(3, Duration::minutes(5), t0())
                        .await
                        .unwrap();
                    if got.is_empty() {
                        return ids;
                    }
                    ids.extend(got.into_iter().map(|c| c.delivery.id));
                }
            })
        })
        .collect();
    let mut all = Vec::new();
    for t in tasks {
        all.extend(t.await.unwrap());
    }
    let unique: HashSet<_> = all.iter().cloned().collect();
    assert_eq!(all.len(), unique.len(), "a delivery was claimed twice");
    assert_eq!(all.len(), EVENTS * 2, "every delivery was claimed once");
}

pub async fn a_lapsed_lease_makes_a_delivery_claimable_again(store: Webhooks) {
    store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    store.append_event("e", PAYLOAD, t0()).await.unwrap();
    let lease = Duration::minutes(5);

    let a = store.claim_due(1, lease, t0()).await.unwrap();
    assert_eq!(a.len(), 1);
    let almost = t0() + lease - Duration::seconds(1);
    assert!(store.claim_due(1, lease, almost).await.unwrap().is_empty());

    let b = store.claim_due(1, lease, t0() + lease).await.unwrap();
    assert_eq!(b.len(), 1, "the crashed claimer's row is claimable");
    assert_eq!(
        b[0].delivery.id, a[0].delivery.id,
        "same delivery id across claims"
    );
    assert_eq!(b[0].delivery.attempt_count, 0, "claiming is not an attempt");
    assert_eq!(b[0].delivery.claimed_until, Some(t0() + lease + lease));
}

pub async fn a_disabled_subscription_is_not_claimed(store: Webhooks) {
    let s = store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    store.append_event("e", PAYLOAD, t0()).await.unwrap();
    let off = SubscriptionUpdate {
        enabled: Some(false),
        ..Default::default()
    };
    store.update_subscription(s.id, off, t0()).await.unwrap();
    let lease = Duration::minutes(5);
    assert!(store.claim_due(10, lease, t0()).await.unwrap().is_empty());
    let on = SubscriptionUpdate {
        enabled: Some(true),
        ..Default::default()
    };
    store.update_subscription(s.id, on, t0()).await.unwrap();
    assert_eq!(store.claim_due(10, lease, t0()).await.unwrap().len(), 1);
}

pub fn the_backoff_is_exponential_bounded_and_ends_after_eight_attempts() {
    let mut total = Duration::zero();
    let mut last = Duration::zero();
    for attempt in 1..MAX_ATTEMPTS {
        let mid = retry_delay(attempt, 0.5).unwrap();
        assert!(mid > last, "delays grow");
        last = mid;
        total += mid;
        let (lo, hi) = (
            retry_delay(attempt, 0.0).unwrap(),
            retry_delay(attempt, 1.0).unwrap(),
        );
        assert!(lo < mid && mid < hi);
        assert!(lo.num_milliseconds() * 10 >= mid.num_milliseconds() * 9 - 1);
        assert!(hi.num_milliseconds() * 10 <= mid.num_milliseconds() * 11 + 1);
    }
    assert_eq!(retry_delay(1, 0.5), Some(Duration::seconds(60)));
    assert!(
        total > Duration::hours(20) && total < Duration::hours(26),
        "{total}"
    );
    assert_eq!(retry_delay(MAX_ATTEMPTS, 0.5), None);
    assert_eq!(retry_delay(0, 0.5), None);
}

pub async fn failures_are_rescheduled_with_backoff_then_the_delivery_fails(store: Webhooks) {
    let s = store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    store.append_event("e", PAYLOAD, t0()).await.unwrap();
    let lease = Duration::minutes(1);
    let jitter = FixedJitter(0.5);
    let mut now = t0();
    let mut id = String::new();

    for attempt in 1..=MAX_ATTEMPTS {
        let got = store.claim_due(1, lease, now).await.unwrap();
        assert_eq!(got.len(), 1, "attempt {attempt} is due");
        id = got[0].delivery.id.clone();
        let d = store
            .record_failure(&id, Some(500), "boom", now, &jitter)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(d.attempt_count, attempt);
        assert_eq!(d.last_status_code, Some(500));
        assert_eq!(d.last_error.as_deref(), Some("boom"));
        assert!(d.claimed_until.is_none());
        match retry_delay(attempt, 0.5) {
            Some(wait) => {
                assert_eq!(d.status, DeliveryStatus::Pending);
                assert_eq!(d.next_attempt_at, now + wait);
                assert!(
                    store
                        .claim_due(1, lease, d.next_attempt_at - Duration::seconds(1))
                        .await
                        .unwrap()
                        .is_empty()
                );
                now = d.next_attempt_at;
            }
            None => {
                assert_eq!(d.status, DeliveryStatus::Failed);
                assert_eq!(d.failed_at, Some(now));
            }
        }
    }
    let far = now + Duration::days(10);
    assert!(store.claim_due(1, lease, far).await.unwrap().is_empty());

    let rows = store
        .list_deliveries(
            s.id,
            &DeliveryQuery {
                status: Some(DeliveryStatus::Failed),
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, id);

    // A late result for a settled delivery changes nothing.
    let again = store
        .mark_delivered(&id, Some(200), far)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.status, DeliveryStatus::Failed);
    assert_eq!(again.attempt_count, MAX_ATTEMPTS);
    assert!(
        store
            .record_failure(
                "00000000-0000-4000-8000-000000000000",
                None,
                "x",
                far,
                &jitter
            )
            .await
            .unwrap()
            .is_none()
    );
}

pub async fn a_delivered_delivery_is_settled(store: Webhooks) {
    let s = store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    store.append_event("e", PAYLOAD, t0()).await.unwrap();
    let got = store
        .claim_due(1, Duration::minutes(1), t0())
        .await
        .unwrap();
    let id = got[0].delivery.id.clone();
    let at = t0() + Duration::seconds(3);
    let d = store
        .mark_delivered(&id, Some(204), at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.status, DeliveryStatus::Delivered);
    assert_eq!((d.attempt_count, d.last_status_code), (1, Some(204)));
    assert_eq!(d.delivered_at, Some(at));
    assert!(d.claimed_until.is_none());
    let far = t0() + Duration::days(1);
    assert!(
        store
            .claim_due(1, Duration::minutes(1), far)
            .await
            .unwrap()
            .is_empty()
    );

    let q = DeliveryQuery {
        status: Some(DeliveryStatus::Pending),
        limit: 10,
        ..Default::default()
    };
    assert!(store.list_deliveries(s.id, &q).await.unwrap().is_empty());
}

pub async fn redeliver_resets_a_delivery_and_keeps_its_id(store: Webhooks) {
    let s = store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    store.append_event("e", PAYLOAD, t0()).await.unwrap();
    let mut now = t0();
    let mut id = String::new();
    for _ in 0..MAX_ATTEMPTS {
        let got = store.claim_due(1, Duration::minutes(1), now).await.unwrap();
        id = got[0].delivery.id.clone();
        let d = store
            .record_failure(&id, Some(502), "bad gateway", now, &FixedJitter(0.0))
            .await
            .unwrap()
            .unwrap();
        now = d.next_attempt_at.max(now);
    }
    let failed = store
        .list_deliveries(
            s.id,
            &DeliveryQuery {
                limit: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(failed[0].status, DeliveryStatus::Failed);

    let later = now + Duration::hours(2);
    let d = store.redeliver(&id, later).await.unwrap().unwrap();
    assert_eq!(d.id, id);
    assert_eq!(d.status, DeliveryStatus::Pending);
    assert_eq!(d.attempt_count, 0);
    assert_eq!(d.next_attempt_at, later);
    assert!(d.claimed_until.is_none() && d.failed_at.is_none() && d.last_error.is_none());
    assert!(d.last_status_code.is_none());
    let got = store
        .claim_due(1, Duration::minutes(1), later)
        .await
        .unwrap();
    assert_eq!(got[0].delivery.id, id);
    assert!(
        store
            .redeliver("00000000-0000-4000-8000-000000000000", later)
            .await
            .unwrap()
            .is_none()
    );
}

pub async fn purge_removes_events_and_deliveries_past_retention(store: Webhooks) {
    let s = store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    let old = store.append_event("e", PAYLOAD, t0()).await.unwrap();
    let fresh_at = t0() + retention() - Duration::days(1);
    let fresh = store.append_event("e", PAYLOAD, fresh_at).await.unwrap();

    let now = t0() + retention();
    assert_eq!(
        store
            .purge(retention(), now - Duration::seconds(1))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .purge(retention(), now + Duration::seconds(1))
            .await
            .unwrap(),
        1
    );
    let rows = store
        .list_deliveries(
            s.id,
            &DeliveryQuery {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event_id, fresh.id);
    assert_ne!(rows[0].event_id, old.id);
    assert!(store.get_subscription(s.id).await.unwrap().is_some());
    assert_eq!(
        store
            .purge(retention(), now + Duration::days(40))
            .await
            .unwrap(),
        1
    );
    assert!(
        store
            .list_deliveries(
                s.id,
                &DeliveryQuery {
                    limit: 10,
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .is_empty()
    );
}

pub async fn a_subscription_holds_at_most_two_active_secrets(store: Webhooks) {
    let s = store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    let other = store
        .create_subscription(sub("o", &[], true), t0())
        .await
        .unwrap();
    assert!(
        store
            .add_secret(999_999, secret(9), t0())
            .await
            .unwrap()
            .is_none()
    );

    let a = store
        .add_secret(s.id, secret(1), t0())
        .await
        .unwrap()
        .unwrap();
    let b = store
        .add_secret(
            s.id,
            NewSecret {
                secret: vec![2; 48],
                encrypted: true,
                key_id: Some("k1".into()),
            },
            t0() + Duration::minutes(1),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (a.secret.clone(), a.encrypted, a.key_id.clone()),
        (vec![1; 32], false, None)
    );
    assert_eq!(
        (b.secret.clone(), b.encrypted, b.key_id.clone()),
        (vec![2; 48], true, Some("k1".to_string()))
    );
    assert_eq!(a.created_at, t0());

    let err = store.add_secret(s.id, secret(3), t0()).await.unwrap_err();
    assert_eq!(err.code(), "invalid_request");
    assert!(
        store
            .add_secret(other.id, secret(4), t0())
            .await
            .unwrap()
            .is_some()
    );

    let active = store.list_active_secrets(s.id).await.unwrap();
    assert_eq!(
        active.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![a.id, b.id]
    );

    assert!(
        !store.retire_secret(other.id, a.id, t0()).await.unwrap(),
        "wrong subscription"
    );
    assert!(store.retire_secret(s.id, a.id, t0()).await.unwrap());
    assert!(
        !store.retire_secret(s.id, a.id, t0()).await.unwrap(),
        "already retired"
    );
    let active = store.list_active_secrets(s.id).await.unwrap();
    assert_eq!(active.iter().map(|x| x.id).collect::<Vec<_>>(), vec![b.id]);

    let c = store
        .add_secret(s.id, secret(5), t0())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.list_active_secrets(s.id).await.unwrap().len(), 2);
    assert_ne!(c.id, a.id);
}

pub async fn racing_secret_additions_never_exceed_two_active(store: Webhooks) {
    let s = store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    let tasks: Vec<_> = (0..6u8)
        .map(|i| {
            let store = Arc::clone(&store);
            tokio::spawn(async move { store.add_secret(s.id, secret(i), t0()).await })
        })
        .collect();
    let mut added = 0;
    for t in tasks {
        if matches!(t.await.unwrap(), Ok(Some(_))) {
            added += 1;
        }
    }
    assert_eq!(added, 2);
    assert_eq!(store.list_active_secrets(s.id).await.unwrap().len(), 2);
}

pub async fn deliveries_list_newest_event_first_and_page(store: Webhooks) {
    let s = store
        .create_subscription(sub("s", &[], true), t0())
        .await
        .unwrap();
    let mut events = Vec::new();
    for _ in 0..5 {
        events.push(store.append_event("e", PAYLOAD, t0()).await.unwrap().id);
    }
    let page = store
        .list_deliveries(
            s.id,
            &DeliveryQuery {
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.iter().map(|d| d.event_id).collect::<Vec<_>>(),
        vec![events[4], events[3]]
    );
    let next = store
        .list_deliveries(
            s.id,
            &DeliveryQuery {
                limit: 10,
                before_event: Some(events[3]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        next.iter().map(|d| d.event_id).collect::<Vec<_>>(),
        vec![events[2], events[1], events[0]]
    );
}

pub async fn limit_events_admit_each_owner_and_kind_only_after_an_hour(store: Webhooks) {
    use crate::{Clock, ManualClock};

    let clock = ManualClock::new(t0());
    let all = store
        .create_subscription(sub("all", &[], true), t0())
        .await
        .unwrap();
    let matching = store
        .create_subscription(sub("limits", &["owner.limit_reached"], true), t0())
        .await
        .unwrap();
    let other = store
        .create_subscription(sub("other", &["repository.created"], true), t0())
        .await
        .unwrap();
    let off = store
        .create_subscription(sub("off", &[], false), t0())
        .await
        .unwrap();
    let mut ids = HashSet::new();
    for (advance, admitted, count) in [
        (Duration::zero(), true, 8),
        (Duration::zero(), false, 8),
        (Duration::hours(1) - Duration::microseconds(1), false, 8),
        (Duration::microseconds(1), true, 16),
        (Duration::hours(1) + Duration::microseconds(1), true, 24),
    ] {
        clock.advance(advance);
        for owner in ["alice", "studio"] {
            for kind in ["repositories", "members", "storage_bytes", "locks"] {
                let event = store
                    .append_limit_reached(&UserId::new(owner), kind, "1", clock.now())
                    .await
                    .unwrap();
                assert_eq!(
                    event.is_some(),
                    admitted,
                    "{owner}/{kind} at {}",
                    clock.now()
                );
                if let Some(event) = event {
                    assert!(ids.insert(event.id));
                    assert_eq!(event.event_type, "owner.limit_reached");
                    assert_eq!(event.created_at, clock.now());
                    assert_eq!(event.payload, "1");
                }
            }
        }
        let q = DeliveryQuery {
            limit: 100,
            ..Default::default()
        };
        for (s, expected) in [(&all, count), (&matching, count), (&other, 0), (&off, 0)] {
            assert_eq!(
                store.list_deliveries(s.id, &q).await.unwrap().len(),
                expected,
                "{}",
                s.name
            );
        }
    }
}

pub async fn racing_limit_admissions_append_exactly_one_event(store: Webhooks) {
    let s = store
        .create_subscription(sub("limits", &[], true), t0())
        .await
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .append_limit_reached(&UserId::new("alice"), "repositories", "1", t0())
                .await
                .unwrap()
        }));
    }
    let mut admitted = 0;
    for task in tasks {
        admitted += usize::from(task.await.unwrap().is_some());
    }
    assert_eq!(admitted, 1);
    assert_eq!(
        store
            .list_deliveries(
                s.id,
                &DeliveryQuery {
                    limit: 10,
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .len(),
        1
    );
}

/// Store-contract recorder for the repository-zero refusal exercised below.
pub struct RepositoryLimitEvents(pub Webhooks);

#[async_trait::async_trait]
impl crate::LimitEvents for RepositoryLimitEvents {
    async fn record(&self, event: crate::LimitReached, now: DateTime<Utc>) -> crate::Result<()> {
        assert_eq!(event.kind, crate::LimitKind::Repositories);
        assert_eq!(event.value, 0);
        self.0
            .append_limit_reached(&event.owner, "repositories", "1", now)
            .await?;
        Ok(())
    }

    fn draft(
        &self,
        event: crate::DomainEvent,
        now: DateTime<Utc>,
    ) -> crate::Result<crate::EventDraft> {
        crate::MemoryLimitEvents::new().draft(event, now)
    }
}

pub async fn racing_limit_services_record_one_event(
    meta: super::Store,
    access_store: super::Access,
    hooks: [Webhooks; 2],
) {
    use crate::memory::{MemoryAuditStore, MemoryObjectStore};
    use crate::{
        AccessConfig, AccessService, Clock, Credential, Identity, Limits, ManualClock,
        Repositories, Rules, WebhookSettings,
    };

    let clock = Arc::new(ManualClock::new(t0()));
    let sub = hooks[0]
        .create_subscription(sub("limits", &["owner.limit_reached"], true), t0())
        .await
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for (i, hook_store) in hooks.iter().enumerate() {
        let access = Arc::new(
            AccessService::new(
                access_store.clone(),
                clock.clone(),
                Arc::new(RepositoryLimitEvents(hook_store.clone())),
            )
            .with_config(AccessConfig {
                default_limits: Limits {
                    repositories: Some(0),
                    ..Default::default()
                },
                ..AccessConfig::default()
            })
            .with_registry(meta.clone())
            .with_webhooks(hook_store.clone(), WebhookSettings::default()),
        );
        let repos = Repositories::new(
            meta.clone(),
            Arc::new(MemoryObjectStore::new()),
            Arc::new(MemoryAuditStore::new()),
            access,
            clock.clone(),
            Rules::empty(),
        );
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            let who = Identity {
                user: UserId::new("alice"),
                credential: Credential::Session,
            };
            barrier.wait().await;
            let err = repos
                .create(&who, &who.user, &format!("repo{i}"), None, None)
                .await
                .unwrap_err();
            assert!(matches!(
                err,
                crate::PynError::RepoLimitReached { limit: 0, .. }
            ));
            assert_eq!(err.code(), "repo_limit_reached");
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let due = hooks[0]
        .claim_due(10, Duration::minutes(1), clock.now())
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].subscription.id, sub.id);
    assert_eq!(due[0].event.created_at, clock.now());
    assert_eq!(due[0].event.payload, "1");
}

/// Generates one `#[tokio::test]` per webhook-store case for the store built by `$factory`.
#[macro_export]
macro_rules! webhook_contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; limit_events_admit_each_owner_and_kind_only_after_an_hour);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; racing_limit_admissions_append_exactly_one_event);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; subscriptions_are_created_listed_updated_and_deleted);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; fan_out_follows_the_event_type_filter_and_the_enabled_flag);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; a_delivery_is_claimed_only_when_due_and_only_by_one_claimer);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; racing_claimers_never_receive_the_same_delivery);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; a_lapsed_lease_makes_a_delivery_claimable_again);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; a_disabled_subscription_is_not_claimed);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; failures_are_rescheduled_with_backoff_then_the_delivery_fails);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; a_delivered_delivery_is_settled);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; redeliver_resets_a_delivery_and_keeps_its_id);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; purge_removes_events_and_deliveries_past_retention);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; a_subscription_holds_at_most_two_active_secrets);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; racing_secret_additions_never_exceed_two_active);
        $crate::webhook_contract_tests!(@one $factory; [$(#[$attr])*]; deliveries_list_newest_event_first_and_page);
        #[test]
        fn the_backoff_is_exponential_bounded_and_ends_after_eight_attempts() {
            $crate::contract::the_backoff_is_exponential_bounded_and_ends_after_eight_attempts();
        }
    };
    (@one $factory:expr; [$(#[$attr:meta])*]; $name:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        $(#[$attr])*
        async fn $name() {
            let store: $crate::contract::Webhooks = ($factory).await;
            $crate::contract::$name(store).await;
        }
    };
}

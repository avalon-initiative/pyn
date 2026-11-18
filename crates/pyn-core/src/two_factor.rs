//! Second factors (decision #1097): authenticator-app factors, single-use recovery codes, the delay that grows with
//! failed attempts, and the fresh-check window that gates sensitive actions.

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

use crate::access_service::AccessStore;
use crate::error::{PynError, Result};
use crate::types::UserId;
use crate::webhook::NewSecret;
use crate::webhook_secret::SecretKey;

/// An account holds at most this many confirmed factors.
pub const MAX_FACTORS: usize = 5;
pub const RECOVERY_CODE_COUNT: usize = 10;
/// How long a passed second factor keeps sensitive actions open on the credential that passed it.
pub const FRESH_MINUTES: i64 = 15;
pub const MAX_LABEL: usize = 64;

/// Attempts at a code allowed before the delay starts, the first delay and its ceiling, and the quiet time after which
/// the count starts over.
pub const FREE_ATTEMPTS: u32 = 5;
const BASE_DELAY_SECS: i64 = 30;
const MAX_DELAY_SECS: i64 = 3600;
const QUIET_RESET_HOURS: i64 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorKind {
    Totp,
}

impl FactorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Totp => "totp",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        (text == "totp").then_some(Self::Totp)
    }
}

/// A registered second factor. A factor stays pending (`confirmed_at` unset) until its first code is proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactorRecord {
    pub id: String,
    pub user: UserId,
    pub kind: FactorKind,
    pub label: String,
    /// The seed, sealed under the server key when one is set (`encrypted`).
    pub secret: Vec<u8>,
    pub encrypted: bool,
    pub key_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub confirmed_at: Option<DateTime<Utc>>,
    /// The latest time step accepted, so a code cannot be replayed.
    pub last_step: Option<i64>,
}

/// The outcome of removing a factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorRemoval {
    NotFound,
    /// No confirmed factor remains: the recovery codes and the delay state went with it.
    Removed {
        second_factor_left: bool,
    },
}

/// The failed-attempt state of an account's factor checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AttemptState {
    pub attempts: u32,
    pub locked_until: Option<DateTime<Utc>>,
    pub last_at: Option<DateTime<Utc>>,
}

/// Counts an attempt before it is checked. Refused (with the end of the delay) while a delay runs; otherwise the attempt
/// is allowed, and from the fifth on the next one waits 30 seconds, doubling up to an hour. A success clears the state.
pub fn count_attempt(
    mut state: AttemptState,
    now: DateTime<Utc>,
) -> (AttemptState, std::result::Result<(), DateTime<Utc>>) {
    if let Some(until) = state.locked_until.filter(|until| *until > now) {
        return (state, Err(until));
    }
    if state
        .last_at
        .is_some_and(|last| now - last > Duration::hours(QUIET_RESET_HOURS))
    {
        state = AttemptState::default();
    }
    state.attempts = state.attempts.saturating_add(1);
    state.last_at = Some(now);
    state.locked_until = (state.attempts >= FREE_ATTEMPTS).then(|| {
        let doublings = (state.attempts - FREE_ATTEMPTS).min(16);
        let secs = (BASE_DELAY_SECS << doublings).min(MAX_DELAY_SECS);
        now + Duration::seconds(secs)
    });
    (state, Ok(()))
}

pub fn too_many(until: DateTime<Utc>, now: DateTime<Utc>) -> PynError {
    PynError::TooManyAttempts {
        retry_after_secs: (until - now).num_seconds().max(1),
    }
}

/// Recovery codes are ten characters of lowercase base32 (50 bits), shown as `xxxxx-xxxxx`.
const CODE_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

pub fn generate_recovery_codes() -> Result<Vec<String>> {
    (0..RECOVERY_CODE_COUNT)
        .map(|_| {
            let mut raw = [0u8; 10];
            getrandom::fill(&mut raw)
                .map_err(|e| PynError::Storage(format!("random source: {e}")))?;
            let text: String = raw
                .iter()
                .map(|b| CODE_ALPHABET[(b & 31) as usize] as char)
                .collect();
            Ok(format!("{}-{}", &text[..5], &text[5..]))
        })
        .collect()
}

/// A typed recovery code in its stored form: lowercase, without the dash or spaces.
pub fn normalize_recovery_code(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether `text` has the shape of a recovery code rather than an authenticator code.
pub fn looks_like_recovery_code(text: &str) -> bool {
    let code = normalize_recovery_code(text);
    code.len() == 10 && code.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The hash a recovery code is stored under.
pub fn hash_recovery_code(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"pyn-recovery-code-v1:");
    hasher.update(normalize_recovery_code(text).as_bytes());
    hex::encode(hasher.finalize())
}

pub fn validate_label(label: &str) -> Result<String> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL || label.chars().any(char::is_control)
    {
        return Err(PynError::InvalidRequest(format!(
            "a factor name is 1 to {MAX_LABEL} characters"
        )));
    }
    Ok(label.to_string())
}

const SEED_AAD: &[u8] = b"pyn-totp-seed-v1";

/// A seed as the store keeps it: encrypted under the server key when one is set, so a database leak alone is not a seed
/// leak.
pub fn seal_seed(key: Option<&SecretKey>, seed: &[u8]) -> Result<NewSecret> {
    match key {
        Some(key) => Ok(NewSecret {
            secret: key.seal_bytes(seed, SEED_AAD)?,
            encrypted: true,
            key_id: Some(key.key_id().to_string()),
        }),
        None => Ok(NewSecret {
            secret: seed.to_vec(),
            encrypted: false,
            key_id: None,
        }),
    }
}

/// The seed of a stored factor. Fails when it is encrypted and `key` is missing or is not the one that sealed it.
pub fn reveal_seed(key: Option<&SecretKey>, factor: &FactorRecord) -> Result<Vec<u8>> {
    if !factor.encrypted {
        return Ok(factor.secret.clone());
    }
    let key = key.ok_or_else(|| {
        PynError::Storage("a two-factor seed is encrypted but no PYN_SECRET_KEY is set".into())
    })?;
    if factor.key_id.as_deref() != Some(key.key_id()) {
        return Err(PynError::Storage(
            "a two-factor seed was encrypted under a different PYN_SECRET_KEY".into(),
        ));
    }
    key.open_bytes(&factor.secret, SEED_AAD)
}

/// Re-encrypts every stored seed from `current` (plaintext rows included) to `new` in one atomic store step. Fails,
/// changing nothing, when `current` cannot open every row. Returns how many were rewritten.
pub async fn rotate_seed_key(
    store: &dyn AccessStore,
    current: Option<&SecretKey>,
    new: &SecretKey,
) -> Result<u64> {
    store
        .reseal_factor_seeds(&|row| seal_seed(Some(new), &reveal_seed(current, row)?))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn the_delay_starts_at_the_fifth_attempt_and_doubles() {
        let mut state = AttemptState::default();
        let mut now = at(0);
        for n in 1..FREE_ATTEMPTS {
            let (next, allowed) = count_attempt(state, now);
            assert!(allowed.is_ok());
            assert_eq!(next.locked_until, None, "attempt {n}");
            state = next;
        }
        let (next, allowed) = count_attempt(state, now);
        assert!(allowed.is_ok(), "the fifth attempt is still checked");
        assert_eq!(next.locked_until, Some(now + Duration::seconds(30)));
        state = next;
        assert_eq!(
            count_attempt(state, now).1,
            Err(now + Duration::seconds(30))
        );
        now += Duration::seconds(30);
        let (next, allowed) = count_attempt(state, now);
        assert!(allowed.is_ok());
        assert_eq!(next.locked_until, Some(now + Duration::seconds(60)));
        state = next;
        for _ in 0..20 {
            now = state.locked_until.unwrap();
            state = count_attempt(state, now).0;
        }
        assert_eq!(
            state.locked_until,
            Some(now + Duration::seconds(MAX_DELAY_SECS))
        );
    }

    #[test]
    fn a_refused_attempt_changes_nothing() {
        let state = AttemptState {
            attempts: 6,
            locked_until: Some(at(100)),
            last_at: Some(at(0)),
        };
        let (after, refused) = count_attempt(state, at(50));
        assert_eq!(after, state);
        assert_eq!(refused, Err(at(100)));
    }

    #[test]
    fn a_long_quiet_spell_starts_the_count_over() {
        let state = AttemptState {
            attempts: 9,
            locked_until: Some(at(10)),
            last_at: Some(at(0)),
        };
        let (after, allowed) = count_attempt(state, at(25 * 3600));
        assert!(allowed.is_ok());
        assert_eq!((after.attempts, after.locked_until), (1, None));
    }

    #[test]
    fn recovery_codes_are_distinct_and_hash_by_their_normal_form() {
        let codes = generate_recovery_codes().unwrap();
        assert_eq!(codes.len(), RECOVERY_CODE_COUNT);
        let unique: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), RECOVERY_CODE_COUNT);
        for code in &codes {
            assert_eq!(code.len(), 11);
            assert!(looks_like_recovery_code(code));
            assert_eq!(
                hash_recovery_code(code),
                hash_recovery_code(&code.to_uppercase())
            );
            assert_eq!(
                hash_recovery_code(code),
                hash_recovery_code(&code.replace('-', " "))
            );
        }
        assert_ne!(hash_recovery_code(&codes[0]), hash_recovery_code(&codes[1]));
    }

    #[test]
    fn an_authenticator_code_does_not_look_like_a_recovery_code() {
        assert!(!looks_like_recovery_code("123456"));
        assert!(!looks_like_recovery_code("12345678"));
    }

    #[test]
    fn labels_are_trimmed_and_bounded() {
        assert_eq!(validate_label("  phone ").unwrap(), "phone");
        assert!(validate_label("   ").is_err());
        assert!(validate_label(&"x".repeat(MAX_LABEL + 1)).is_err());
        assert!(validate_label("a\nb").is_err());
    }

    fn factor(sealed: NewSecret) -> FactorRecord {
        FactorRecord {
            id: "f".into(),
            user: UserId::new("alice"),
            kind: FactorKind::Totp,
            label: "phone".into(),
            secret: sealed.secret,
            encrypted: sealed.encrypted,
            key_id: sealed.key_id,
            created_at: at(0),
            confirmed_at: None,
            last_step: None,
        }
    }

    #[test]
    fn a_sealed_seed_hides_the_seed_and_needs_its_own_key() {
        let key = SecretKey::from_hex(&"07".repeat(32)).unwrap();
        let other = SecretKey::from_hex(&"08".repeat(32)).unwrap();
        let seed = [0x5au8; 20];
        let row = factor(seal_seed(Some(&key), &seed).unwrap());
        assert!(row.encrypted);
        assert!(!row.secret.windows(seed.len()).any(|w| w == seed));
        assert_eq!(reveal_seed(Some(&key), &row).unwrap(), seed);
        assert!(reveal_seed(Some(&other), &row).is_err());
        assert!(reveal_seed(None, &row).is_err());
        let mut flipped = row.clone();
        *flipped.secret.last_mut().unwrap() ^= 1;
        assert!(reveal_seed(Some(&key), &flipped).is_err());
    }

    #[test]
    fn a_seed_without_a_key_is_stored_as_is() {
        let row = factor(seal_seed(None, &[1, 2, 3]).unwrap());
        assert!(!row.encrypted);
        assert_eq!(reveal_seed(None, &row).unwrap(), [1, 2, 3]);
    }
}

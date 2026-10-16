//! Bounds on how many uploads run at once, per user and for the whole process.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::{PynError, Result};
use crate::types::UserId;

pub const DEFAULT_UPLOADS_PER_USER: u32 = 8;
pub const DEFAULT_UPLOADS_PER_SERVER: u32 = 64;

#[derive(Default)]
struct Counts {
    total: u32,
    users: HashMap<UserId, u32>,
}

/// Admits uploads up to a per-user and a per-process count; a permit frees its slot when dropped.
pub struct UploadGate {
    per_user: u32,
    per_server: u32,
    counts: Arc<Mutex<Counts>>,
}

pub struct UploadPermit {
    user: UserId,
    counts: Arc<Mutex<Counts>>,
}

impl UploadGate {
    pub fn new(per_user: u32, per_server: u32) -> Self {
        Self {
            per_user,
            per_server,
            counts: Arc::default(),
        }
    }

    /// `UploadsBusy` when `user` or the process is at its limit.
    pub fn admit(&self, user: &UserId) -> Result<UploadPermit> {
        let mut counts = self.counts.lock().unwrap();
        if counts.total >= self.per_server {
            return Err(PynError::UploadsBusy {
                scope: "the server",
            });
        }
        let mine = counts.users.get(user).copied().unwrap_or(0);
        if mine >= self.per_user {
            return Err(PynError::UploadsBusy { scope: "this user" });
        }
        counts.total += 1;
        counts.users.insert(user.clone(), mine + 1);
        Ok(UploadPermit {
            user: user.clone(),
            counts: self.counts.clone(),
        })
    }
}

impl Default for UploadGate {
    fn default() -> Self {
        Self::new(DEFAULT_UPLOADS_PER_USER, DEFAULT_UPLOADS_PER_SERVER)
    }
}

impl Drop for UploadPermit {
    fn drop(&mut self) {
        let mut counts = self.counts.lock().unwrap();
        counts.total = counts.total.saturating_sub(1);
        if let Some(n) = counts.users.get_mut(&self.user) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                counts.users.remove(&self.user);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_each_limit_and_frees_on_drop() {
        let gate = UploadGate::new(2, 3);
        let (a, b) = (UserId::new("a"), UserId::new("b"));
        let p1 = gate.admit(&a).unwrap();
        let _p2 = gate.admit(&a).unwrap();
        assert!(matches!(
            gate.admit(&a),
            Err(PynError::UploadsBusy { scope: "this user" })
        ));
        let _p3 = gate.admit(&b).unwrap();
        assert!(matches!(
            gate.admit(&b),
            Err(PynError::UploadsBusy {
                scope: "the server"
            })
        ));
        drop(p1);
        gate.admit(&b).unwrap();
    }
}

//! `pyn commit -q` on a pyn server: the commit paths print through `say!`, which a set flag silences.

use std::sync::atomic::{AtomicBool, Ordering};

static ON: AtomicBool = AtomicBool::new(false);

pub fn set() {
    ON.store(true, Ordering::Relaxed);
}

pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

macro_rules! say {
    ($($arg:tt)*) => {
        if !$crate::quiet::on() {
            println!($($arg)*);
        }
    };
}
pub(crate) use say;

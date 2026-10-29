//! Per-thread operation counts of the real object database, read by tests that prove lookups stay lazy.

use std::cell::Cell;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub pack_opens: usize,
    pub pack_enumerations: usize,
    pub pack_reads: usize,
    pub id_scans: usize,
    pub abbreviation_lookups: usize,
    pub object_counts: usize,
    pub loose_fanout_reads: usize,
    pub config_scans: usize,
}

thread_local! {
    static COUNTS: Cell<Counts> = const { Cell::new(Counts {
        pack_opens: 0,
        pack_enumerations: 0,
        pack_reads: 0,
        id_scans: 0,
        abbreviation_lookups: 0,
        object_counts: 0,
        loose_fanout_reads: 0,
        config_scans: 0,
    }) };
}

pub(crate) fn bump(f: impl FnOnce(&mut Counts)) {
    COUNTS.with(|c| {
        let mut counts = c.get();
        f(&mut counts);
        c.set(counts);
    });
}

/// Returns this thread's counts and resets them.
pub fn take() -> Counts {
    COUNTS.with(Cell::take)
}

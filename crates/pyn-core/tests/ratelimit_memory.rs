use std::sync::Arc;

use pyn_core::contract::RateLimits;
use pyn_core::memory::MemoryRateLimitStore;

async fn memory() -> RateLimits {
    Arc::new(MemoryRateLimitStore::new())
}

pyn_core::rate_limit_contract_tests!(memory());

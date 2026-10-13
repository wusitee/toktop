//! toktop core: reads AI coding tool logs and aggregates token usage.

pub mod aggregate;
pub mod config;
pub mod format;
pub mod model;
pub mod parse;
pub mod paths;
pub mod pricing;
pub mod store;
pub mod watch;
pub mod worker;

use pricing::PriceTable;
use std::path::PathBuf;
use std::time::SystemTime;

pub fn prices_cache_path() -> PathBuf {
    paths::cache_dir().join("prices.json")
}

/// The cached price table and whether it is due for a refresh.
pub fn cached_prices() -> (PriceTable, bool) {
    match PriceTable::load(&prices_cache_path()) {
        Some((table, modified)) => {
            let stale = SystemTime::now()
                .duration_since(modified)
                .map_or(true, |age| age > pricing::MAX_AGE);
            (table, stale)
        }
        None => (PriceTable::default(), true),
    }
}

/// Downloads fresh prices and caches them. Blocking.
pub fn refresh_prices() -> Result<PriceTable, Box<dyn std::error::Error + Send + Sync>> {
    let table = PriceTable::fetch()?;
    table.save(&prices_cache_path())?;
    Ok(table)
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

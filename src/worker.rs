//! Background thread that owns the store, applies file changes and publishes
//! summaries. The UI talks to it only through [`Msg`] and the `emit` callback.

use crate::aggregate::{self, Period, Summary};
use crate::model::Tool;
use crate::pricing::PriceTable;
use crate::store::Store;
use crate::{paths, watch};
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub enum Msg {
    Changed(Vec<PathBuf>),
    Rescan,
    WatchCoverage(bool),
    Period(Period),
    Prices(PriceTable),
    /// Download prices now instead of waiting for the daily refresh.
    RefreshPrices,
}

/// Quiet time after a change before it is applied.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Upper bound on how long continuous writes can delay an update.
const MAX_WAIT: Duration = Duration::from_secs(1);
/// Recompute interval for time-dependent values (token rate, day rollover).
const TICK: Duration = Duration::from_secs(30);
/// Full rescan interval when inotify is unavailable.
const POLL: Duration = Duration::from_secs(10);
const PRICE_RETRY: Duration = Duration::from_secs(60 * 60);

/// Starts the worker. `emit` runs on the worker thread whenever the summary
/// for the selected period changes, and on every clock tick.
pub fn spawn(
    period: Period,
    tools: Vec<Tool>,
    emit: impl FnMut(Summary) + Send + 'static,
) -> io::Result<Sender<Msg>> {
    let (tx, rx) = mpsc::channel();
    let state = State {
        store: Store::with_tools(tools),
        prices: PriceTable::default(),
        period,
        last: None,
        emit,
    };
    let fetch_tx = tx.clone();
    std::thread::Builder::new()
        .name("toktop-worker".into())
        .spawn(move || state.run(rx, fetch_tx))?;
    Ok(tx)
}

/// Returns memory freed after a scan to the OS. A parallel scan leaves most
/// of its transient allocations in per-thread glibc arenas otherwise.
pub fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: malloc_trim has no preconditions.
        unsafe {
            malloc_trim(0);
        }
    }
}

struct State<E> {
    store: Store,
    prices: PriceTable,
    period: Period,
    last: Option<Summary>,
    emit: E,
}

impl<E: FnMut(Summary)> State<E> {
    fn publish(&mut self, tick: bool) {
        let summary = aggregate::summarize(&self.store, &self.prices, self.period, crate::now_ms());
        if tick || self.last.as_ref() != Some(&summary) {
            self.last = Some(summary.clone());
            (self.emit)(summary);
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Msg>, tx: Sender<Msg>) {
        let tools = self.store.tools.clone();
        let mut roots: Vec<PathBuf> = tools
            .iter()
            .copied()
            .flat_map(paths::source_roots)
            .collect();
        roots.sort();
        roots.dedup();
        let watch_tx = tx.clone();
        // Watch before scanning so writes during the scan are queued.
        let watcher = watch::spawn(roots, paths::title_sources(&tools), move |change| {
            let _ = watch_tx.send(match change {
                watch::Change::Paths(paths) => Msg::Changed(paths),
                watch::Change::Rescan => Msg::Rescan,
                watch::Change::Coverage(complete) => Msg::WatchCoverage(complete),
            });
        });
        if let Err(error) = &watcher {
            eprintln!("toktop: file watching unavailable ({error}); polling every {POLL:?}");
        }
        // Coverage is reported by the watcher after registration.
        let mut watching = false;
        let (prices, stale) = crate::cached_prices();
        self.prices = prices;
        self.store.sync_all();
        self.publish(false);
        trim_heap();

        let mut next_price_fetch = Some(if stale {
            Instant::now()
        } else {
            Instant::now() + crate::pricing::MAX_AGE
        });
        let mut pending: HashSet<PathBuf> = HashSet::new();
        let mut rescan = false;
        let mut first_change: Option<Instant> = None;
        let mut last_change = Instant::now();
        let mut next_tick = Instant::now() + TICK;
        let mut next_poll = Instant::now() + POLL;

        loop {
            let now = Instant::now();
            if next_price_fetch.is_some_and(|at| at <= now) {
                next_price_fetch = Some(now + PRICE_RETRY);
                let tx = tx.clone();
                std::thread::spawn(move || match crate::refresh_prices() {
                    Ok(table) => {
                        let _ = tx.send(Msg::Prices(table));
                    }
                    Err(error) => eprintln!("toktop: price refresh failed: {error}"),
                });
            }

            let mut deadline = next_tick;
            if let Some(first) = first_change {
                deadline = deadline.min((last_change + DEBOUNCE).min(first + MAX_WAIT));
            }
            if !watching {
                deadline = deadline.min(next_poll);
            }
            if let Some(at) = next_price_fetch {
                deadline = deadline.min(at);
            }

            match rx.recv_timeout(deadline.saturating_duration_since(now)) {
                Ok(Msg::Changed(paths)) => {
                    pending.extend(paths);
                    last_change = Instant::now();
                    first_change.get_or_insert(last_change);
                }
                Ok(Msg::Rescan) => {
                    rescan = true;
                    last_change = Instant::now();
                    first_change.get_or_insert(last_change);
                }
                Ok(Msg::WatchCoverage(complete)) => watching = complete,
                Ok(Msg::Period(period)) => {
                    self.period = period;
                    self.publish(false);
                }
                Ok(Msg::Prices(table)) => {
                    self.prices = table;
                    next_price_fetch = Some(Instant::now() + crate::pricing::MAX_AGE);
                    self.publish(false);
                    trim_heap();
                }
                Ok(Msg::RefreshPrices) => next_price_fetch = Some(Instant::now()),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }

            let now = Instant::now();
            if let Some(first) = first_change
                && (now >= last_change + DEBOUNCE || now >= first + MAX_WAIT)
            {
                first_change = None;
                let changed = if std::mem::take(&mut rescan) {
                    // The watcher has already reconciled its watches.
                    pending.clear();
                    let changed = self.store.sync_all();
                    trim_heap();
                    changed
                } else {
                    self.store.update(pending.drain())
                };
                if changed {
                    self.publish(false);
                }
            }
            if !watching && now >= next_poll {
                next_poll = now + POLL;
                if let Ok(watcher) = &watcher {
                    watcher.reconcile();
                }
                if self.store.sync_all() {
                    self.publish(false);
                }
            }
            if now >= next_tick {
                next_tick = now + TICK;
                self.publish(true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_summary_is_published_on_clock_tick() {
        let mut published = Vec::new();
        let mut state = State {
            store: Store::default(),
            prices: PriceTable::default(),
            period: Period::AllTime,
            last: None,
            emit: |summary| published.push(summary),
        };
        state.publish(false);
        state.publish(false);
        state.publish(true);
        drop(state);
        assert_eq!(published.len(), 2);
        assert_eq!(published[0], published[1]);
    }
}

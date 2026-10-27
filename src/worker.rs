//! Background thread that owns the store, applies file changes and publishes
//! summaries. The UI talks to it only through [`Msg`] and the `emit` callback.

use crate::aggregate::{self, Period, Summary};
use crate::model::Tool;
use crate::pricing::PriceTable;
use crate::store::Store;
use crate::{paths, watch};
use chrono::NaiveDate;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub enum Msg {
    Changed(Vec<PathBuf>),
    Rescan,
    /// Watcher generation and whether it covers every directory.
    WatchCoverage(u32, bool),
    Period(Period),
    /// Retain full model/project breakdowns for this day only while open.
    DetailDay(Option<NaiveDate>),
    Prices(PriceTable),
    /// Download prices now instead of waiting for the daily refresh.
    RefreshPrices,
    /// Read only these tools from now on. Data of other tools is dropped
    /// and their directories are no longer watched or scanned.
    Tools(Vec<Tool>),
    Remote(String, Vec<Tool>, Result<crate::remote::Snapshot, String>),
}

/// Quiet time after a change before it is applied.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Upper bound on how long continuous writes can delay an update.
const MAX_WAIT: Duration = Duration::from_secs(1);
/// Recompute interval for token rate, rolling periods aging out and day rollover.
const TICK: Duration = Duration::from_secs(30);
/// Full rescan interval when inotify is unavailable.
const POLL: Duration = Duration::from_secs(10);
const PRICE_RETRY: Duration = Duration::from_secs(60 * 60);

/// Starts the worker. `emit` runs on the worker thread whenever the summary
/// for the selected period changes, and on every clock tick.
pub fn spawn(
    period: Period,
    tools: Vec<Tool>,
    remote: Vec<crate::config::Remote>,
    emit: impl FnMut(Summary) + Send + 'static,
) -> io::Result<Sender<Msg>> {
    let (tx, rx) = mpsc::channel();
    let state = State {
        store: Store::with_tools(tools),
        prices: PriceTable::default(),
        period,
        detail_day: None,
        last: None,
        emit,
        remote: remote
            .into_iter()
            .map(|remote| {
                (
                    remote.host,
                    RemoteState {
                        error: Some("waiting for first refresh".to_owned()),
                        last_success: None,
                    },
                )
            })
            .collect(),
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
    detail_day: Option<NaiveDate>,
    last: Option<Summary>,
    emit: E,
    remote: HashMap<String, RemoteState>,
}

struct RemoteState {
    error: Option<String>,
    last_success: Option<i64>,
}

impl<E: FnMut(Summary)> State<E> {
    fn apply_remote(&mut self, host: String, result: Result<crate::remote::Snapshot, String>) {
        let state = self.remote.get_mut(&host).expect("configured remote");
        match result {
            Ok(snapshot) => {
                self.store.replace_remote(&host, snapshot.sessions);
                state.error = None;
                state.last_success = Some(crate::now_ms());
            }
            Err(error) => state.error = Some(error),
        }
    }

    fn publish(&mut self, tick: bool) {
        let mut summary = aggregate::summarize_with_day(
            &self.store,
            &self.prices,
            self.period,
            crate::now_ms(),
            self.detail_day,
        );
        summary.remote_errors = self
            .remote
            .iter()
            .filter_map(|(host, state)| {
                state.error.as_ref().map(|error| {
                    let age = state.last_success.map_or_else(
                        || "no data yet".to_owned(),
                        |time| {
                            format!("last updated {}", crate::format::ago(time, crate::now_ms()))
                        },
                    );
                    format!("{host}: {error} ({age})")
                })
            })
            .collect();
        summary.remote_errors.sort();
        if tick || self.last.as_ref() != Some(&summary) {
            self.last = Some(summary.clone());
            (self.emit)(summary);
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Msg>, tx: Sender<Msg>) {
        // Watch before scanning so writes during the scan are queued.
        let mut generation = 0;
        let mut watcher = start_watch(&self.store.tools, &tx, generation);
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
        let mut next_remote = Instant::now();
        let mut remote_busy = HashSet::new();

        loop {
            let now = Instant::now();
            if now >= next_remote {
                next_remote = now + Duration::from_secs(60);
                for host in self.remote.keys() {
                    if remote_busy.insert(host.clone()) {
                        let host = host.clone();
                        let tx = tx.clone();
                        let tools = self.store.tools.clone();
                        std::thread::spawn(move || {
                            let result = crate::remote::fetch(&host, &tools);
                            let _ = tx.send(Msg::Remote(host, tools, result));
                        });
                    }
                }
            }
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
            if !self.remote.is_empty() {
                deadline = deadline.min(next_remote);
            }
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
                Ok(Msg::WatchCoverage(from, complete)) if from == generation => watching = complete,
                // Reported by a replaced watcher.
                Ok(Msg::WatchCoverage(..)) => {}
                Ok(Msg::Period(period)) => {
                    self.period = period;
                    self.publish(false);
                }
                Ok(Msg::DetailDay(day)) => {
                    self.detail_day = day;
                    self.publish(false);
                }
                Ok(Msg::Prices(table)) => {
                    self.prices = table;
                    next_price_fetch = Some(Instant::now() + crate::pricing::MAX_AGE);
                    self.publish(false);
                    trim_heap();
                }
                Ok(Msg::RefreshPrices) => next_price_fetch = Some(Instant::now()),
                Ok(Msg::Tools(tools)) if tools != self.store.tools => {
                    // Stop the old watcher before its changes can be queued.
                    drop(watcher);
                    generation += 1;
                    watcher = start_watch(&tools, &tx, generation);
                    watching = false;
                    pending.clear();
                    rescan = false;
                    first_change = None;
                    let remote = std::mem::take(&mut self.store.remote);
                    self.store = Store::with_tools(tools);
                    self.store.remote = remote;
                    for files in self.store.remote.values_mut() {
                        files.retain(|_, entry| self.store.tools.contains(&entry.tool));
                    }
                    for state in self.remote.values_mut() {
                        state.error = Some("refreshing after tool selection changed".into());
                    }
                    next_remote = Instant::now();
                    self.store.sync_all();
                    self.publish(false);
                    trim_heap();
                }
                Ok(Msg::Tools(_)) => {}
                Ok(Msg::Remote(host, tools, result)) => {
                    remote_busy.remove(&host);
                    if tools != self.store.tools {
                        next_remote = Instant::now();
                        continue;
                    }
                    self.apply_remote(host, result);
                    self.publish(false);
                }
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

/// Watches the source roots and title indexes of `tools`, forwarding
/// changes to the worker. Coverage reports carry `generation`.
fn start_watch(tools: &[Tool], tx: &Sender<Msg>, generation: u32) -> io::Result<watch::Handle> {
    let mut roots: Vec<PathBuf> = tools
        .iter()
        .copied()
        .flat_map(paths::source_roots)
        .collect();
    roots.sort();
    roots.dedup();
    let tx = tx.clone();
    let watcher = watch::spawn(
        roots,
        paths::title_sources(tools),
        tools.to_vec(),
        move |change| {
            let _ = tx.send(match change {
                watch::Change::Paths(paths) => Msg::Changed(paths),
                watch::Change::Rescan => Msg::Rescan,
                watch::Change::Coverage(complete) => Msg::WatchCoverage(generation, complete),
            });
        },
    );
    if let Err(error) = &watcher {
        eprintln!("toktop: file watching unavailable ({error}); polling every {POLL:?}");
    }
    watcher
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
            detail_day: None,
            last: None,
            remote: HashMap::new(),
            emit: |summary| published.push(summary),
        };
        state.publish(false);
        state.publish(false);
        state.publish(true);
        drop(state);
        assert_eq!(published.len(), 2);
        assert_eq!(published[0], published[1]);
    }

    #[test]
    fn remote_error_retains_snapshot_and_reports_its_age() {
        let mut published = Vec::new();
        let mut state = State {
            store: Store::default(),
            prices: PriceTable::default(),
            period: Period::AllTime,
            detail_day: None,
            last: None,
            emit: |summary| published.push(summary),
            remote: HashMap::from([(
                "dev".to_owned(),
                RemoteState {
                    error: None,
                    last_success: None,
                },
            )]),
        };
        let mut session = crate::model::Session {
            id: "a".into(),
            models: vec!["model".into()],
            ..Default::default()
        };
        session.events.push(crate::model::Event {
            ts: crate::now_ms(),
            model: 0,
            tokens: crate::model::Tokens {
                input: 10,
                ..Default::default()
            },
            tier: Default::default(),
            key: None,
            input_is_split: false,
            cache_write_1h_raw: 0,
            duration_ms: 0,
        });
        state.apply_remote(
            "dev".into(),
            Ok(crate::remote::Snapshot {
                version: 1,
                sessions: vec![(Tool::Codex, session)],
            }),
        );
        state.apply_remote("dev".into(), Err("offline".into()));
        state.publish(false);
        drop(state);
        assert_eq!(published[0].total.tokens.input, 10);
        assert!(published[0].remote_errors[0].contains("offline (last updated"));
    }
}

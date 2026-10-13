//! Turns parsed sessions into the totals and breakdowns the UI shows.

use crate::model::{Event, RateLimits, Tokens, Tool};
use crate::pricing::{Price, PriceTable};
use crate::store::Store;
use chrono::{DateTime, Datelike, Days, Local, NaiveDate, TimeZone};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    #[default]
    Today,
    ThisWeek,
    Last7Days,
    ThisMonth,
    Last30Days,
    AllTime,
}

impl Period {
    pub const ALL: [Period; 6] = [
        Period::Today,
        Period::ThisWeek,
        Period::Last7Days,
        Period::ThisMonth,
        Period::Last30Days,
        Period::AllTime,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Period::Today => "Today",
            Period::ThisWeek => "This week",
            Period::Last7Days => "Last 7 days",
            Period::ThisMonth => "This month",
            Period::Last30Days => "Last 30 days",
            Period::AllTime => "All time",
        }
    }

    /// First local day included in the period.
    pub fn start(self, today: NaiveDate) -> NaiveDate {
        match self {
            Period::Today => today,
            Period::ThisWeek => today - Days::new(today.weekday().num_days_from_monday() as u64),
            Period::Last7Days => today - Days::new(6),
            Period::ThisMonth => today.with_day(1).unwrap_or(today),
            Period::Last30Days => today - Days::new(29),
            Period::AllTime => NaiveDate::MIN,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub tokens: Tokens,
    pub cost: f64,
    pub requests: u64,
}

impl Usage {
    fn add(&mut self, tokens: &Tokens, cost: f64) {
        self.tokens += tokens;
        self.cost += cost;
        self.requests += 1;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Stable model id or full project path, independent of the display label.
    pub key: String,
    pub label: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub tool: Tool,
    pub title: String,
    pub project: Option<String>,
    pub last_activity: i64,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DayRow {
    pub date: NaiveDate,
    pub usage: Usage,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    pub period: Period,
    pub total: Usage,
    /// False when no price table is loaded; costs must be hidden.
    pub prices_available: bool,
    pub by_tool: Vec<(Tool, Usage)>,
    pub by_model: Vec<Row>,
    pub by_project: Vec<Row>,
    pub sessions: Vec<SessionRow>,
    /// One entry per local day with usage, across all time, oldest first.
    pub daily: Vec<DayRow>,
    /// Tokens per minute over the last five minutes.
    pub rate_per_min: f64,
    pub codex_limits: Option<RateLimits>,
    /// Models with usage in the period but no known price.
    pub unpriced: Vec<String>,
}

pub const RATE_WINDOW_MS: i64 = 5 * 60 * 1000;

fn local_date(ts: i64) -> NaiveDate {
    DateTime::from_timestamp_millis(ts)
        .map(|time| time.with_timezone(&Local).date_naive())
        .unwrap_or_default()
}

fn sorted_rows(map: HashMap<String, Usage>) -> Vec<Row> {
    let mut rows: Vec<Row> = map
        .into_iter()
        .map(|(key, usage)| Row {
            label: key.clone(),
            key,
            usage,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.usage
            .tokens
            .total()
            .cmp(&a.usage.tokens.total())
            .then_with(|| a.label.cmp(&b.label))
    });
    rows
}

/// Folder name of a project path, for display.
pub fn project_label(project: &str) -> String {
    let trimmed = project.trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
        .to_owned()
}

pub fn summarize(store: &Store, prices: &PriceTable, period: Period, now_ms: i64) -> Summary {
    let today = local_date(now_ms);
    let start = period.start(today);
    let start_ms = if period == Period::AllTime {
        i64::MIN
    } else {
        start
            .and_hms_opt(0, 0, 0)
            .and_then(|midnight| Local.from_local_datetime(&midnight).earliest())
            .map_or(i64::MIN, |time| time.timestamp_millis())
    };

    let mut summary = Summary {
        period,
        prices_available: !prices.is_empty(),
        ..Summary::default()
    };
    let mut by_tool: HashMap<Tool, Usage> = HashMap::new();
    let mut by_model: HashMap<String, Usage> = HashMap::new();
    let mut by_project: HashMap<String, Usage> = HashMap::new();
    let mut daily: HashMap<NaiveDate, Usage> = HashMap::new();
    let mut unpriced: HashSet<String> = HashSet::new();
    let mut price_cache: HashMap<&str, Option<Price>> = HashMap::new();
    let mut recent_tokens = 0u64;

    // Iterate in a stable order so cross-file duplicates always resolve to the
    // same file.
    let mut files: Vec<_> = store.files.iter().collect();
    files.sort_by(|a, b| a.0.cmp(b.0));

    // A response copied into several files (resumed or forked sessions) is
    // counted once, at its first copy, with the most complete counts seen.
    let mut merged: HashMap<(Tool, &str), Event> = HashMap::new();
    for (_, entry) in &files {
        for event in &entry.session.events {
            if let Some(key) = event.key.as_deref() {
                merged
                    .entry((entry.tool, key))
                    .and_modify(|merged| merged.max_merge(event))
                    .or_insert_with(|| event.clone());
            }
        }
    }

    for (_, entry) in files {
        let session = &entry.session;
        let mut session_usage = Usage::default();
        let mut last_activity = i64::MIN;
        for event in &session.events {
            let tokens = match event.key.as_deref() {
                // `remove` makes later copies miss and be skipped.
                Some(key) => match merged.remove(&(entry.tool, key)) {
                    Some(event) => event.tokens,
                    None => continue,
                },
                None => event.tokens,
            };
            // Raw-only Claude snapshots remain pending until duplicates merge.
            if tokens.is_empty() {
                continue;
            }

            let model = session
                .models
                .get(event.model as usize)
                .map_or("unknown", String::as_str);
            let price = *price_cache
                .entry(model)
                .or_insert_with(|| prices.lookup(model).copied());
            let cost = price.map_or(0.0, |price| price.cost(&tokens, event.tier));

            daily
                .entry(local_date(event.ts))
                .or_default()
                .add(&tokens, cost);
            if now_ms - event.ts <= RATE_WINDOW_MS && event.ts <= now_ms {
                recent_tokens = recent_tokens.saturating_add(tokens.total());
            }
            if event.ts < start_ms {
                continue;
            }
            if price.is_none() && !prices.is_empty() {
                unpriced.insert(model.to_owned());
            }
            summary.total.add(&tokens, cost);
            by_tool.entry(entry.tool).or_default().add(&tokens, cost);
            by_model
                .entry(model.to_owned())
                .or_default()
                .add(&tokens, cost);
            if let Some(project) = &session.project {
                by_project
                    .entry(project.trim_end_matches('/').to_owned())
                    .or_default()
                    .add(&tokens, cost);
            }
            session_usage.add(&tokens, cost);
            last_activity = last_activity.max(event.ts);
        }

        if let Some(limits) = &session.limits
            && summary
                .codex_limits
                .as_ref()
                .is_none_or(|known| known.observed < limits.observed)
        {
            summary.codex_limits = Some(limits.clone());
        }

        if session_usage.requests > 0 {
            let title = store
                .titles
                .get(&session.id)
                .or(session.title.as_ref())
                .cloned()
                .unwrap_or_else(|| session.id.chars().take(8).collect());
            summary.sessions.push(SessionRow {
                tool: entry.tool,
                title,
                project: session.project.clone(),
                last_activity,
                usage: session_usage,
            });
        }
    }

    let mut tools: Vec<(Tool, Usage)> = by_tool.into_iter().collect();
    tools.sort_by(|a, b| {
        b.1.tokens
            .total()
            .cmp(&a.1.tokens.total())
            .then(a.0.cmp(&b.0))
    });
    summary.by_tool = tools;
    summary.by_model = sorted_rows(by_model);
    summary.by_project = sorted_rows(by_project);
    let paths: Vec<_> = summary
        .by_project
        .iter()
        .map(|row| row.key.clone())
        .collect();
    for row in &mut summary.by_project {
        // Grow the suffix until no other project has the same path components.
        let mut suffix = row.key.as_str();
        for (index, _) in row.key.rmatch_indices('/') {
            suffix = &row.key[index + 1..];
            if !paths.iter().any(|other| {
                other != &row.key && (other == suffix || other.ends_with(&format!("/{suffix}")))
            }) {
                break;
            }
            suffix = &row.key;
        }
        row.label = suffix.to_owned();
    }
    summary
        .sessions
        .sort_by_key(|row| std::cmp::Reverse(row.last_activity));
    let mut days: Vec<DayRow> = daily
        .into_iter()
        .map(|(date, usage)| DayRow { date, usage })
        .collect();
    days.sort_by_key(|day| day.date);
    summary.daily = days;
    summary.rate_per_min = recent_tokens as f64 / (RATE_WINDOW_MS as f64 / 60_000.0);
    let mut unpriced: Vec<String> = unpriced.into_iter().collect();
    unpriced.sort();
    summary.unpriced = unpriced;
    summary
}

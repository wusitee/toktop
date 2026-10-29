//! Turns parsed sessions into the totals and breakdowns the UI shows.

use crate::model::{Event, RateLimits, Session, Tokens, Tool};
use crate::pricing::{Cost, Price, PriceTable};
use crate::store::Store;
use chrono::{DateTime, Days, Local, NaiveDate};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    #[default]
    Last24Hours,
    Last7Days,
    Last30Days,
    AllTime,
}

impl Period {
    pub const ALL: [Period; 4] = [
        Period::Last24Hours,
        Period::Last7Days,
        Period::Last30Days,
        Period::AllTime,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Period::Last24Hours => "Last 24 hours",
            Period::Last7Days => "Last 7 days",
            Period::Last30Days => "Last 30 days",
            Period::AllTime => "All time",
        }
    }

    /// Short name used on the command line and in the config file.
    pub fn key(self) -> &'static str {
        match self {
            Period::Last24Hours => "24h",
            Period::Last7Days => "7d",
            Period::Last30Days => "30d",
            Period::AllTime => "all",
        }
    }

    pub fn from_key(key: &str) -> Option<Period> {
        match key {
            "24h" | "today" => Some(Period::Last24Hours),
            "7d" | "week" => Some(Period::Last7Days),
            "30d" | "month" => Some(Period::Last30Days),
            "all" => Some(Period::AllTime),
            _ => None,
        }
    }

    /// First timestamp included in the period, in milliseconds.
    pub fn start_ms(self, now_ms: i64) -> i64 {
        let days = match self {
            Period::Last24Hours => 1,
            Period::Last7Days => 7,
            Period::Last30Days => 30,
            Period::AllTime => return i64::MIN,
        };
        now_ms.saturating_sub(days * 24 * 60 * 60 * 1000)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub tokens: Tokens,
    pub cost: Cost,
    pub requests: u64,
    /// Output and reasoning tokens of requests with a known duration.
    pub timed_tokens: u64,
    /// Summed duration of those requests, in milliseconds.
    pub timed_ms: u64,
}

impl Usage {
    fn add(&mut self, tokens: &Tokens, cost: Cost, duration_ms: u32) {
        self.tokens += tokens;
        self.cost += cost;
        self.requests += 1;
        if duration_ms > 0 {
            self.timed_tokens = self
                .timed_tokens
                .saturating_add(tokens.output.saturating_add(tokens.reasoning));
            self.timed_ms = self.timed_ms.saturating_add(u64::from(duration_ms));
        }
    }

    fn merge(&mut self, other: &Usage) {
        self.tokens += &other.tokens;
        self.cost += other.cost;
        self.requests += other.requests;
        self.timed_tokens = self.timed_tokens.saturating_add(other.timed_tokens);
        self.timed_ms = self.timed_ms.saturating_add(other.timed_ms);
    }

    /// Average output speed in tokens per second over timed requests.
    pub fn speed(&self) -> Option<f64> {
        (self.timed_ms > 0).then(|| self.timed_tokens as f64 * 1000.0 / self.timed_ms as f64)
    }
}

/// How a row's tokens divide among models, tools, projects and sessions.
/// Each list is sorted by tokens, most first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Split {
    pub models: Vec<(String, u64)>,
    pub tools: Vec<(Tool, u64)>,
    /// Full project paths, as in [`Row::key`].
    pub projects: Vec<(String, u64)>,
    /// Top-level sessions with usage, subagents counted with their parent.
    pub sessions: u32,
}

#[derive(Default)]
struct SplitAcc<'a> {
    models: HashMap<&'a str, u64>,
    tools: HashMap<Tool, u64>,
    projects: HashMap<&'a str, u64>,
    sessions: HashSet<(Tool, &'a str)>,
}

fn add_tokens<K: std::hash::Hash + Eq>(map: &mut HashMap<K, u64>, key: K, tokens: u64) {
    let total = map.entry(key).or_default();
    *total = total.saturating_add(tokens);
}

fn sorted_tokens<K: Ord>(map: HashMap<K, u64>) -> Vec<(K, u64)> {
    let mut items: Vec<(K, u64)> = map.into_iter().collect();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    items
}

impl<'a> SplitAcc<'a> {
    fn add(&mut self, at: &Origin<'a>, tokens: u64) {
        add_tokens(&mut self.models, at.model, tokens);
        add_tokens(&mut self.tools, at.tool, tokens);
        if let Some(project) = at.project {
            add_tokens(&mut self.projects, project, tokens);
        }
        self.sessions.insert((at.tool, at.root));
    }

    fn finish(self) -> Split {
        let owned = |items: Vec<(&str, u64)>| {
            items
                .into_iter()
                .map(|(key, tokens)| (key.to_owned(), tokens))
                .collect()
        };
        Split {
            models: owned(sorted_tokens(self.models)),
            tools: sorted_tokens(self.tools),
            projects: owned(sorted_tokens(self.projects)),
            sessions: self.sessions.len() as u32,
        }
    }
}

/// Where an event's tokens were spent.
struct Origin<'a> {
    tool: Tool,
    model: &'a str,
    /// Project path without trailing `/`.
    project: Option<&'a str>,
    /// Top-level session id.
    root: &'a str,
}

/// Usage and split of one breakdown row or day, while summing.
#[derive(Default)]
struct Acc<'a> {
    usage: Usage,
    split: SplitAcc<'a>,
}

impl<'a> Acc<'a> {
    fn add(&mut self, at: &Origin<'a>, tokens: &Tokens, cost: Cost, duration_ms: u32) {
        self.usage.add(tokens, cost, duration_ms);
        self.split.add(at, tokens.total());
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Stable model id or full project path, independent of the display label.
    pub key: String,
    pub label: String,
    pub usage: Usage,
    pub split: Split,
    /// Average output speed per local day over the [`SPEED_DAYS`] days
    /// ending today, oldest first, for days with timed requests. Filled for
    /// model rows only.
    pub daily_speed: Vec<(NaiveDate, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRow {
    pub tool: Tool,
    pub usage: Usage,
    pub split: Split,
}

/// A session together with the subagent sessions it spawned.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub tool: Tool,
    /// Id of the top-level session.
    pub id: String,
    pub title: String,
    pub project: Option<String>,
    pub first_activity: i64,
    pub last_activity: i64,
    pub usage: Usage,
    /// Tokens per model, most first.
    pub models: Vec<(String, u64)>,
    /// Subagent sessions with usage in the period.
    pub subagents: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DayRow {
    pub date: NaiveDate,
    pub usage: Usage,
    pub split: Split,
}

/// Full breakdowns retained only for the day whose detail card is open.
#[derive(Debug, Clone, PartialEq)]
pub struct DayDetail {
    pub date: NaiveDate,
    pub by_model: Vec<Row>,
    pub by_project: Vec<Row>,
    pub sessions: Vec<SessionRow>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    pub period: Period,
    pub total: Usage,
    /// False when no price table is loaded; costs must be hidden.
    pub prices_available: bool,
    pub by_tool: Vec<ToolRow>,
    pub by_model: Vec<Row>,
    pub by_project: Vec<Row>,
    pub sessions: Vec<SessionRow>,
    /// One entry per local day with usage, across all time, oldest first.
    pub daily: Vec<DayRow>,
    pub day_detail: Option<DayDetail>,
    /// Tokens per minute over the last five minutes.
    pub rate_per_min: f64,
    /// Output speed over the latest [`RECENT_REQUESTS`] timed requests, any
    /// period.
    pub recent_speed: Option<f64>,
    /// Consecutive days with usage, ending today, or yesterday when today
    /// has none yet.
    pub streak_days: u32,
    pub codex_limits: Option<RateLimits>,
    /// Latest local quota snapshot per Codex home; accounts are never merged.
    pub codex_limits_by_home: std::collections::BTreeMap<String, RateLimits>,
    /// Models with usage in the period but no known price.
    pub unpriced: Vec<String>,
    /// Hosts whose latest refresh failed or has not completed.
    pub remote_errors: Vec<String>,
}

pub const RATE_WINDOW_MS: i64 = 5 * 60 * 1000;
/// Timed requests covered by [`Summary::recent_speed`].
pub const RECENT_REQUESTS: usize = 20;
/// Days covered by [`Row::daily_speed`], ending today.
pub const SPEED_DAYS: u64 = 30;

fn local_date(ts: i64) -> NaiveDate {
    DateTime::from_timestamp_millis(ts)
        .map(|time| time.with_timezone(&Local).date_naive())
        .unwrap_or_default()
}

fn sorted_rows(map: HashMap<&str, Acc>) -> Vec<Row> {
    let mut rows: Vec<Row> = map
        .into_iter()
        .map(|(key, acc)| Row {
            key: key.to_owned(),
            label: key.to_owned(),
            usage: acc.usage,
            split: acc.split.finish(),
            daily_speed: Vec::new(),
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

fn label_projects(rows: &mut [Row]) {
    let paths: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
    let labels = project_labels(&paths);
    for (row, label) in rows.iter_mut().zip(labels) {
        row.label = label;
    }
}

/// Folder name of a project path, for display.
pub fn project_label(project: &str) -> String {
    let (host, project) = project
        .split_once(':')
        .filter(|(host, path)| !host.contains('/') && path.starts_with('/'))
        .map_or((None, project), |(host, path)| (Some(host), path));
    let trimmed = project.trim_end_matches('/');
    let label = trimmed
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed);
    host.map_or_else(|| label.to_owned(), |host| format!("{label} [{host}]"))
}

/// Display labels for project paths: each path's basename, extended by the
/// fewest parent components that tell it apart from the other paths.
pub fn project_labels(paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .map(|path| {
            // Grow the suffix until no other project has the same path components.
            let mut suffix = *path;
            for (index, _) in path.rmatch_indices('/') {
                suffix = &path[index + 1..];
                if !paths.iter().any(|other| {
                    other != path && (*other == suffix || other.ends_with(&format!("/{suffix}")))
                }) {
                    break;
                }
                suffix = path;
            }
            if let Some((host, _)) = path
                .split_once(':')
                .filter(|(host, rest)| !host.contains('/') && rest.starts_with('/'))
            {
                format!(
                    "{} [{host}]",
                    suffix.strip_prefix(&format!("{host}:")).unwrap_or(suffix)
                )
            } else {
                suffix.to_owned()
            }
        })
        .collect()
}

#[derive(Default)]
struct Group<'a> {
    usage: Usage,
    first_activity: i64,
    last_activity: i64,
    models: HashMap<&'a str, u64>,
    subagents: u32,
    /// Title or first prompt of the top-level session, when its file has
    /// usage.
    title: Option<&'a str>,
    /// Title of a subagent, used when the top-level session has none.
    fallback_title: Option<&'a str>,
    project: Option<&'a str>,
}

impl<'a> Group<'a> {
    fn metadata(&mut self, session: &'a Session, root_id: &str) {
        if root_id == session.id {
            self.title = session.title.as_deref().or(session.prompt.as_deref());
            self.project = session.project.as_deref();
        } else {
            self.subagents += 1;
            self.fallback_title = self
                .fallback_title
                .or(session.title.as_deref())
                .or(session.prompt.as_deref());
            self.project = self.project.or(session.project.as_deref());
        }
    }
}

fn session_rows(
    groups: HashMap<(Tool, &str), Group>,
    metadata: &HashMap<(Tool, &str), &Session>,
    titles: &HashMap<String, String>,
) -> Vec<SessionRow> {
    let mut rows = Vec::with_capacity(groups.len());
    for ((tool, id), group) in groups {
        let root = metadata.get(&(tool, id));
        let title = titles
            .get(id)
            .map(String::as_str)
            .or(group.title)
            .or(root.and_then(|root| root.title.as_deref().or(root.prompt.as_deref())))
            .or(group.fallback_title)
            .map_or_else(|| id.chars().take(8).collect(), str::to_owned);
        let project = root
            .and_then(|root| root.project.as_deref())
            .or(group.project);
        rows.push(SessionRow {
            tool,
            id: id.to_owned(),
            title,
            project: project.map(str::to_owned),
            first_activity: group.first_activity,
            last_activity: group.last_activity,
            usage: group.usage,
            models: sorted_tokens(group.models)
                .into_iter()
                .map(|(model, tokens)| (model.to_owned(), tokens))
                .collect(),
            subagents: group.subagents,
        });
    }
    rows.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

/// Consecutive days with usage ending at `today`, or at yesterday when
/// `today` has none. `daily` is sorted by date.
pub fn streak(daily: &[DayRow], today: NaiveDate) -> u32 {
    let mut expected = today;
    let mut count = 0;
    for day in daily.iter().rev() {
        if day.date > expected {
            continue;
        }
        if day.date != expected {
            if count == 0 && expected == today && day.date == today - Days::new(1) {
                expected = day.date;
            } else {
                break;
            }
        }
        count += 1;
        expected = expected - Days::new(1);
    }
    count
}

pub fn summarize(store: &Store, prices: &PriceTable, period: Period, now_ms: i64) -> Summary {
    summarize_with_day(store, prices, period, now_ms, None)
}

/// Includes model and project breakdowns for one local day, independently
/// of the main period, in the same event pass as the regular summary.
pub fn summarize_with_day<'a>(
    store: &'a Store,
    prices: &PriceTable,
    period: Period,
    now_ms: i64,
    detail_day: Option<NaiveDate>,
) -> Summary {
    let today = local_date(now_ms);
    let start_ms = period.start_ms(now_ms);

    let mut summary = Summary {
        period,
        prices_available: !prices.is_empty(),
        ..Summary::default()
    };
    let mut by_tool: HashMap<Tool, Acc> = HashMap::new();
    let mut by_model: HashMap<&str, Acc> = HashMap::new();
    let mut by_project: HashMap<&str, Acc> = HashMap::new();
    let mut daily: HashMap<NaiveDate, Acc> = HashMap::new();
    let mut day_models: HashMap<&str, Acc> = HashMap::new();
    let mut day_projects: HashMap<&str, Acc> = HashMap::new();
    let speed_start = today - Days::new(SPEED_DAYS - 1);
    let mut model_days: HashMap<(&str, NaiveDate), Usage> = HashMap::new();
    let mut unpriced: HashSet<String> = HashSet::new();
    let mut price_cache: HashMap<&str, Option<Price>> = HashMap::new();
    let mut recent_tokens = 0u64;
    // Latest timed requests as (time, output tokens, duration), oldest on top.
    let mut recent_timed: BinaryHeap<Reverse<(i64, u64, u32)>> = BinaryHeap::new();

    // Iterate in a stable order so cross-file duplicates always resolve to the
    // same file.
    let mut files: Vec<_> = store
        .files
        .iter()
        .chain(
            store
                .remote
                .values()
                .flat_map(|files| files.iter())
                .filter(|(_, entry)| store.tools.contains(&entry.tool)),
        )
        .collect();
    files.sort_by(|a, b| a.0.cmp(b.0));

    // Intern display keys so model/effort pairs can share the existing breakdowns.
    let mut model_labels: HashMap<&str, HashMap<Option<String>, String>> = HashMap::new();
    for (_, entry) in &files {
        for event in &entry.session.events {
            let model = entry
                .session
                .models
                .get(event.model as usize)
                .map_or("unknown", String::as_str);
            let efforts = model_labels.entry(model).or_default();
            if !efforts.contains_key(&event.effort) {
                let label = event
                    .effort
                    .as_deref()
                    .map_or_else(|| model.to_owned(), |effort| format!("{model} · {effort}"));
                efforts.insert(event.effort.clone(), label);
            }
        }
    }

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

    // Session metadata by id, independent of usage in the period, so a row
    // whose subagents alone have usage still shows its own title.
    let mut metadata: HashMap<(Tool, &str), &Session> = HashMap::new();
    for (_, entry) in &files {
        let session = &entry.session;
        let known = metadata
            .entry((entry.tool, session.id.as_str()))
            .or_insert(session);
        if known.title.is_none() && known.prompt.is_none() {
            *known = session;
        }
    }

    // Subagent sessions are grouped under their top-level session.
    let parents: HashMap<(Tool, &str), &str> = files
        .iter()
        .filter_map(|(_, entry)| {
            let parent = entry.session.parent.as_deref()?;
            Some(((entry.tool, entry.session.id.as_str()), parent))
        })
        .collect();
    let root = |tool: Tool, mut id: &'a str| {
        // Bounded, in case of a parent cycle.
        for _ in 0..16 {
            match parents.get(&(tool, id)) {
                Some(parent) => id = parent,
                None => break,
            }
        }
        id
    };
    let mut groups: HashMap<(Tool, &str), Group> = HashMap::new();
    let mut day_groups: HashMap<(Tool, &str), Group> = HashMap::new();

    for (_, entry) in files {
        let session = &entry.session;
        let root_id = root(entry.tool, &session.id);
        let project = session.project.as_deref().map(|project| {
            crate::config::project_name(&store.project_group, project.trim_end_matches('/'))
        });
        let mut session_usage = Usage::default();
        let mut model_tokens: HashMap<&str, u64> = HashMap::new();
        let mut first_activity = i64::MAX;
        let mut last_activity = i64::MIN;
        let mut day_used = false;
        for event in &session.events {
            let merged_event = event
                .key
                .as_deref()
                .and_then(|key| merged.remove(&(entry.tool, key)));
            if event.key.is_some() && merged_event.is_none() {
                continue;
            }
            let effective = merged_event.as_ref().unwrap_or(event);
            let tokens = effective.tokens;
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
            let cost = price.map_or_else(Cost::default, |price| price.cost(&tokens, event.tier));

            let duration = effective.duration_ms;
            let raw_model = model;
            let model = model_labels
                .get(model)
                .and_then(|efforts| efforts.get(&effective.effort))
                .map_or(model, String::as_str);
            let at = Origin {
                tool: entry.tool,
                model,
                project,
                root: root_id,
            };
            let date = local_date(event.ts);
            daily
                .entry(date)
                .or_default()
                .add(&at, &tokens, cost, duration);
            if detail_day == Some(date) {
                day_used = true;
                let group = day_groups
                    .entry((entry.tool, root_id))
                    .or_insert_with(|| Group {
                        first_activity: i64::MAX,
                        ..Group::default()
                    });
                group.usage.add(&tokens, cost, duration);
                group.first_activity = group.first_activity.min(event.ts);
                group.last_activity = group.last_activity.max(event.ts);
                add_tokens(&mut group.models, model, tokens.total());
                day_models
                    .entry(model)
                    .or_default()
                    .add(&at, &tokens, cost, duration);
                if let Some(project) = project {
                    day_projects
                        .entry(project)
                        .or_default()
                        .add(&at, &tokens, cost, duration);
                }
            }
            if duration > 0 && date >= speed_start && date <= today {
                model_days
                    .entry((model, date))
                    .or_default()
                    .add(&tokens, cost, duration);
            }
            if event.ts <= now_ms && now_ms.saturating_sub(event.ts) <= RATE_WINDOW_MS {
                recent_tokens = recent_tokens.saturating_add(tokens.total());
            }
            if duration > 0 && event.ts <= now_ms {
                let output = tokens.output.saturating_add(tokens.reasoning);
                recent_timed.push(Reverse((event.ts, output, duration)));
                if recent_timed.len() > RECENT_REQUESTS {
                    recent_timed.pop();
                }
            }
            if event.ts < start_ms {
                continue;
            }
            // Auto-review is free, so a missing catalog price is expected.
            if price.is_none() && !prices.is_empty() && raw_model != "codex-auto-review" {
                unpriced.insert(raw_model.to_owned());
            }
            summary.total.add(&tokens, cost, duration);
            by_tool
                .entry(entry.tool)
                .or_default()
                .add(&at, &tokens, cost, duration);
            by_model
                .entry(model)
                .or_default()
                .add(&at, &tokens, cost, duration);
            if let Some(project) = project {
                by_project
                    .entry(project)
                    .or_default()
                    .add(&at, &tokens, cost, duration);
            }
            session_usage.add(&tokens, cost, duration);
            add_tokens(&mut model_tokens, model, tokens.total());
            first_activity = first_activity.min(event.ts);
            last_activity = last_activity.max(event.ts);
        }

        if let Some(limits) = &session.limits {
            let home = session
                .codex_home
                .as_deref()
                .unwrap_or("default")
                .to_owned();
            let latest = summary
                .codex_limits_by_home
                .entry(home)
                .or_insert_with(|| limits.clone());
            if latest.observed < limits.observed {
                *latest = limits.clone();
            }
        }

        if day_used {
            day_groups
                .get_mut(&(entry.tool, root_id))
                .unwrap()
                .metadata(session, root_id);
        }
        if session_usage.requests > 0 {
            let group = groups
                .entry((entry.tool, root_id))
                .or_insert_with(|| Group {
                    first_activity: i64::MAX,
                    ..Group::default()
                });
            group.usage.merge(&session_usage);
            group.first_activity = group.first_activity.min(first_activity);
            group.last_activity = group.last_activity.max(last_activity);
            for (model, tokens) in model_tokens {
                add_tokens(&mut group.models, model, tokens);
            }
            group.metadata(session, root_id);
        }
    }

    summary.codex_limits = summary
        .codex_limits_by_home
        .values()
        .max_by_key(|limits| limits.observed)
        .cloned();
    summary.sessions = session_rows(groups, &metadata, &store.titles);
    for row in &mut summary.sessions {
        row.project = row
            .project
            .as_deref()
            .map(|path| crate::config::project_name(&store.project_group, path).to_owned());
    }

    let mut tools: Vec<ToolRow> = by_tool
        .into_iter()
        .map(|(tool, acc)| ToolRow {
            tool,
            usage: acc.usage,
            split: acc.split.finish(),
        })
        .collect();
    tools.sort_by(|a, b| {
        b.usage
            .tokens
            .total()
            .cmp(&a.usage.tokens.total())
            .then(a.tool.cmp(&b.tool))
    });
    summary.by_tool = tools;
    summary.by_model = sorted_rows(by_model);
    let mut speeds: HashMap<&str, Vec<(NaiveDate, f64)>> = HashMap::new();
    for ((model, date), usage) in model_days {
        if let Some(speed) = usage.speed() {
            speeds.entry(model).or_default().push((date, speed));
        }
    }
    for row in &mut summary.by_model {
        if let Some(mut days) = speeds.remove(row.key.as_str()) {
            days.sort_by_key(|(date, _)| *date);
            row.daily_speed = days;
        }
    }
    summary.by_project = sorted_rows(by_project);
    label_projects(&mut summary.by_project);
    let mut days: Vec<DayRow> = daily
        .into_iter()
        .map(|(date, acc)| DayRow {
            date,
            usage: acc.usage,
            split: acc.split.finish(),
        })
        .collect();
    days.sort_by_key(|day| day.date);
    summary.streak_days = streak(&days, today);
    summary.daily = days;
    summary.day_detail = detail_day.map(|date| {
        let mut by_project = sorted_rows(day_projects);
        label_projects(&mut by_project);
        DayDetail {
            date,
            by_model: sorted_rows(day_models),
            by_project,
            sessions: session_rows(day_groups, &metadata, &store.titles),
        }
    });
    if let Some(detail) = &mut summary.day_detail {
        for row in &mut detail.sessions {
            row.project = row
                .project
                .as_deref()
                .map(|path| crate::config::project_name(&store.project_group, path).to_owned());
        }
    }
    summary.rate_per_min = recent_tokens as f64 / (RATE_WINDOW_MS as f64 / 60_000.0);
    let recent = recent_timed.into_iter().fold(
        Usage::default(),
        |mut recent, Reverse((_, output, duration))| {
            recent.timed_tokens = recent.timed_tokens.saturating_add(output);
            recent.timed_ms = recent.timed_ms.saturating_add(u64::from(duration));
            recent
        },
    );
    summary.recent_speed = recent.speed();
    let mut unpriced: Vec<String> = unpriced.into_iter().collect();
    unpriced.sort();
    summary.unpriced = unpriced;
    summary
}

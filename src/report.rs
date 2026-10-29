//! Versioned, headless accounting reports over parsed local sessions.

use crate::config::{Config, project_name};
use crate::model::{Event, RateLimits, Session, Tokens, Tool};
use crate::pricing::{Cost, PriceTable};
use crate::store::Store;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Write};
use std::path::PathBuf;

pub const USAGE: &str = "report [--since TIME] [--until TIME] [--tools tool,...]\n  [--project GLOB]... [--session ID]... [--by model|effort|project|session|tool|day]...\n  [--format table|json] [--offline] [--price-file PATH]";

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Default)]
pub struct Options {
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub tools: Option<Vec<Tool>>,
    pub projects: Vec<glob::Pattern>,
    pub sessions: Vec<String>,
    pub by: Vec<String>,
    pub json: bool,
    pub offline: bool,
    pub price_file: Option<PathBuf>,
}

/// Relative times share one reference instant, including across both bounds.
pub fn parse_time(value: &str, now: i64) -> Result<i64, String> {
    if let Ok(time) = DateTime::parse_from_rfc3339(value) {
        return Ok(time.timestamp_millis());
    }
    let (digits, multiplier) = if let Some(digits) = value.strip_suffix('m') {
        (digits, 60_000i64)
    } else if let Some(digits) = value.strip_suffix('h') {
        (digits, 3_600_000)
    } else if let Some(digits) = value.strip_suffix('d') {
        (digits, 86_400_000)
    } else {
        return Err(format!(
            "invalid time `{value}`; expected RFC 3339 or a duration such as 90m, 6h, 2d"
        ));
    };
    let delta = digits
        .parse::<u64>()
        .ok()
        .and_then(|n| i64::try_from(n).ok())
        .and_then(|n| n.checked_mul(multiplier));
    delta
        .and_then(|delta| now.checked_sub(delta))
        .filter(|ts| DateTime::from_timestamp_millis(*ts).is_some())
        .ok_or_else(|| format!("invalid duration `{value}`"))
}

impl Options {
    pub fn parse(args: &[String], now: i64) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if arg == "--offline" {
                options.offline = true;
                continue;
            }
            let (flag, inline) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(k, v)| (k, Some(v)));
            if !matches!(
                flag,
                "--since"
                    | "--until"
                    | "--tools"
                    | "--project"
                    | "--session"
                    | "--by"
                    | "--format"
                    | "--price-file"
            ) {
                return Err(format!("unknown report argument `{arg}`"));
            }
            let value = inline
                .or_else(|| args.next().map(String::as_str))
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag {
                "--since" => options.since = Some(parse_time(value, now)?),
                "--until" => options.until = Some(parse_time(value, now)?),
                "--tools" => options.tools = Some(Tool::parse_list(value)?),
                "--project" => options.projects.push(
                    glob::Pattern::new(value)
                        .map_err(|e| format!("invalid project glob `{value}`: {e}"))?,
                ),
                "--session" => options.sessions.push(value.to_owned()),
                "--by" => {
                    if !matches!(
                        value,
                        "model" | "effort" | "project" | "session" | "tool" | "day"
                    ) {
                        return Err(format!(
                            "invalid grouping `{value}`; expected model, effort, project, session, tool or day"
                        ));
                    }
                    if options.by.iter().any(|by| by == value) {
                        return Err(format!("duplicate grouping `{value}`"));
                    }
                    options.by.push(value.to_owned());
                }
                "--format" => {
                    options.json = match value {
                        "json" => true,
                        "table" => false,
                        _ => {
                            return Err(format!(
                                "invalid format `{value}`; expected table or json"
                            ));
                        }
                    }
                }
                "--price-file" => options.price_file = Some(value.into()),
                _ => unreachable!(),
            }
        }
        if options
            .since
            .zip(options.until)
            .is_some_and(|(since, until)| since > until)
        {
            return Err("--since must be at or before --until".into());
        }
        Ok(options)
    }

    fn contains(&self, ts: i64) -> bool {
        self.since.is_none_or(|since| ts >= since) && self.until.is_none_or(|until| ts <= until)
    }
}

fn timestamp(ts: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ts)
        .expect("validated timestamp")
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[derive(Debug, Serialize)]
pub struct Window {
    since: Option<String>,
    until: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct PriceInfo {
    pub source: String,
    pub fetched_at: Option<String>,
    pub overridden: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct CostUsd {
    input: f64,
    cache_read: f64,
    cache_write: f64,
    output: f64,
    total: f64,
}

#[derive(Debug, Default, Serialize)]
pub struct ReportTokens {
    input: u64,
    cache_read: u64,
    cache_write: u64,
    cache_write_1h: u64,
    output: u64,
    reasoning: u64,
}

impl From<Tokens> for ReportTokens {
    fn from(t: Tokens) -> Self {
        Self {
            input: t.input,
            cache_read: t.cache_read,
            cache_write: t.cache_write,
            cache_write_1h: t.cache_write_1h,
            output: t.output,
            reasoning: t.reasoning,
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Metrics {
    requests: u64,
    sessions: usize,
    response_ms: u64,
    tokens: ReportTokens,
    cost_usd: CostUsd,
}

#[derive(Debug, Serialize)]
pub struct Group {
    key: BTreeMap<String, Option<String>>,
    #[serde(flatten)]
    metrics: Metrics,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    groups: Vec<Group>,
}

#[derive(Default)]
struct Acc {
    tokens: Tokens,
    cost: Cost,
    requests: u64,
    response_ms: u64,
    sessions: BTreeSet<(Tool, String)>,
    children: BTreeMap<Option<String>, Acc>,
}

impl Acc {
    fn add(&mut self, tool: Tool, id: &str, event: &Event, cost: Cost, keys: &[Option<String>]) {
        self.tokens += &event.tokens;
        self.cost += cost;
        self.requests += 1;
        self.response_ms = self
            .response_ms
            .saturating_add(u64::from(event.duration_ms));
        self.sessions.insert((tool, id.to_owned()));
        if let Some((key, rest)) = keys.split_first() {
            self.children
                .entry(key.clone())
                .or_default()
                .add(tool, id, event, cost, rest);
        }
    }

    fn finish(
        self,
        by: &[String],
        parent: &BTreeMap<String, Option<String>>,
    ) -> (Metrics, Vec<Group>) {
        let metrics = Metrics {
            requests: self.requests,
            sessions: self.sessions.len(),
            response_ms: self.response_ms,
            tokens: self.tokens.into(),
            cost_usd: CostUsd {
                input: self.cost.input,
                cache_read: self.cost.cache_read,
                cache_write: self.cost.cache_write,
                output: self.cost.output,
                total: self.cost.total(),
            },
        };
        let groups = self
            .children
            .into_iter()
            .map(|(value, child)| {
                let mut key = parent.clone();
                key.insert(by[0].clone(), value);
                let (metrics, groups) = child.finish(&by[1..], &key);
                Group {
                    key,
                    metrics,
                    groups,
                }
            })
            .collect();
        (metrics, groups)
    }
}

#[derive(Debug, Serialize)]
pub struct Observation {
    at: String,
    used_percent: f64,
}
#[derive(Debug, Serialize)]
pub struct QuotaWindow {
    minutes: u64,
    first: Observation,
    last: Observation,
    resets_at: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct CodexLimits {
    home: String,
    plan: Option<String>,
    windows: Vec<QuotaWindow>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    schema: u32,
    window: Window,
    pub prices: PriceInfo,
    pub total: Metrics,
    pub groups: Vec<Group>,
    pub unpriced: Vec<String>,
    pub codex_limits: Vec<CodexLimits>,
}

fn quota(sessions: &[(Tool, Session)], options: &Options) -> Vec<CodexLimits> {
    let mut homes: BTreeMap<&str, Vec<&RateLimits>> = BTreeMap::new();
    for (tool, session) in sessions {
        if *tool != Tool::Codex
            || options
                .tools
                .as_ref()
                .is_some_and(|tools| !tools.contains(tool))
        {
            continue;
        }
        let home = session.codex_home.as_deref().unwrap_or("default");
        let observations = if session.limit_history.is_empty() {
            session.limits.iter().collect()
        } else {
            session.limit_history.iter().collect::<Vec<_>>()
        };
        homes.entry(home).or_default().extend(
            observations
                .into_iter()
                .filter(|limits| options.contains(limits.observed)),
        );
    }
    homes
        .into_iter()
        .filter_map(|(home, mut observations)| {
            observations.sort_by_key(|limits| limits.observed);
            let plan = observations.last()?.plan.clone();
            // Codex reset estimates jitter, and concurrent sessions can report
            // slightly decreasing usage within the same reset window.
            const RESET_TOLERANCE_SECONDS: u64 = 60;
            let mut windows: BTreeMap<u64, Vec<(Option<i64>, QuotaWindow)>> = BTreeMap::new();
            for limits in observations {
                for window in [&limits.primary, &limits.secondary].into_iter().flatten() {
                    let observation = || Observation {
                        at: timestamp(limits.observed),
                        used_percent: window.used_percent,
                    };
                    let resets_at = window
                        .resets_at
                        .and_then(|ts| ts.checked_mul(1000))
                        .filter(|ts| DateTime::<Utc>::from_timestamp_millis(*ts).is_some())
                        .map(timestamp);
                    let history = windows.entry(window.window_minutes).or_default();
                    let new_window = history.last().is_none_or(|(reset, known)| {
                        match (*reset, window.resets_at) {
                            (Some(previous), Some(current)) => {
                                previous.abs_diff(current) > RESET_TOLERANCE_SECONDS
                            }
                            // Without reset metadata, a decrease is the only
                            // available indication of a new quota window.
                            (None, None) => window.used_percent < known.last.used_percent,
                            (Some(previous), None) => {
                                window.used_percent < known.last.used_percent
                                    && limits.observed / 1000
                                        > previous.saturating_add(RESET_TOLERANCE_SECONDS as i64)
                            }
                            (None, Some(_)) => window.used_percent < known.last.used_percent,
                        }
                    });
                    if new_window {
                        history.push((
                            window.resets_at,
                            QuotaWindow {
                                minutes: window.window_minutes,
                                first: observation(),
                                last: observation(),
                                resets_at,
                            },
                        ));
                    } else if let Some((reset, known)) = history.last_mut() {
                        known.last = observation();
                        if window.resets_at.is_some() {
                            *reset = window.resets_at;
                            known.resets_at = resets_at;
                        }
                    }
                }
            }
            Some(CodexLimits {
                home: home.to_owned(),
                plan,
                windows: windows
                    .into_values()
                    .flatten()
                    .map(|(_, window)| window)
                    .collect(),
            })
        })
        .collect()
}

pub fn build(
    sessions: &[(Tool, Session)],
    config: &Config,
    prices: &PriceTable,
    options: &Options,
) -> Report {
    // Merge before filtering, preserving the canonical first copy's attribution.
    let mut merged: HashMap<(Tool, &str), Event> = HashMap::new();
    for (tool, session) in sessions {
        for event in &session.events {
            if let Some(key) = event.key.as_deref() {
                merged
                    .entry((*tool, key))
                    .and_modify(|known| known.max_merge(event))
                    .or_insert_with(|| event.clone());
            }
        }
    }
    let mut total = Acc::default();
    let mut unpriced = BTreeSet::new();
    for (tool, session) in sessions {
        let project = session
            .project
            .as_deref()
            .map(|path| project_name(&config.project_group, path.trim_end_matches('/')));
        let selected = options
            .tools
            .as_ref()
            .is_none_or(|tools| tools.contains(tool))
            && (options.sessions.is_empty() || options.sessions.contains(&session.id))
            && (options.projects.is_empty()
                || options.projects.iter().any(|pattern| {
                    session
                        .project
                        .as_deref()
                        .is_some_and(|path| pattern.matches(path))
                        || project.is_some_and(|name| pattern.matches(name))
                }));
        for original in &session.events {
            let merged = original
                .key
                .as_deref()
                .and_then(|key| merged.remove(&(*tool, key)));
            if original.key.is_some() && merged.is_none() {
                continue;
            }
            let event = merged.as_ref().unwrap_or(original);
            if !selected || event.tokens.is_empty() || !options.contains(event.ts) {
                continue;
            }
            let model = session
                .models
                .get(original.model as usize)
                .map_or("unknown", String::as_str);
            let price = prices.lookup(model);
            if price.is_none() && model != "codex-auto-review" {
                unpriced.insert(model.to_owned());
            }
            let cost =
                price.map_or_else(Cost::default, |price| price.cost(&event.tokens, event.tier));
            let keys: Vec<_> = options
                .by
                .iter()
                .map(|by| match by.as_str() {
                    "model" => Some(model.to_owned()),
                    "effort" => event.effort.clone(),
                    "project" => project.map(str::to_owned),
                    "session" => Some(session.id.clone()),
                    "tool" => Some(tool.key().to_owned()),
                    "day" => DateTime::<Utc>::from_timestamp_millis(event.ts)
                        .map(|time| time.format("%Y-%m-%d").to_string()),
                    _ => unreachable!(),
                })
                .collect();
            total.add(*tool, &session.id, event, cost, &keys);
        }
    }
    let (total, groups) = total.finish(&options.by, &BTreeMap::new());
    Report {
        schema: 1,
        window: Window {
            since: options.since.map(timestamp),
            until: options.until.map(timestamp),
        },
        prices: PriceInfo::default(),
        total,
        groups,
        unpriced: unpriced.into_iter().collect(),
        codex_limits: quota(sessions, options),
    }
}

fn table(report: &Report, out: &mut impl Write) -> io::Result<()> {
    fn row(out: &mut impl Write, label: &str, m: &Metrics) -> io::Result<()> {
        writeln!(
            out,
            "{label:<44} {:>8} {:>8} {:>12} {:>12.6}",
            m.requests, m.sessions, m.response_ms, m.cost_usd.total
        )
    }
    writeln!(
        out,
        "{:<44} {:>8} {:>8} {:>12} {:>12}",
        "Group", "Requests", "Sessions", "Response ms", "Cost USD"
    )?;
    row(out, "Total", &report.total)?;
    fn groups(
        out: &mut impl Write,
        rows: &[Group],
        parent: &BTreeMap<String, Option<String>>,
        depth: usize,
    ) -> io::Result<()> {
        for group in rows {
            let label = group
                .key
                .iter()
                .filter(|(key, _)| !parent.contains_key(*key))
                .map(|(key, value)| format!("{key}={}", value.as_deref().unwrap_or("unknown")))
                .collect::<Vec<_>>()
                .join(" / ");
            row(
                out,
                &format!("{}{label}", "  ".repeat(depth)),
                &group.metrics,
            )?;
            groups(out, &group.groups, &group.key, depth + 1)?;
        }
        Ok(())
    }
    groups(out, &report.groups, &BTreeMap::new(), 1)?;
    let t = &report.total.tokens;
    writeln!(
        out,
        "Tokens: input={}, cache_read={}, cache_write={} (1h={}), output={}, reasoning={}",
        t.input, t.cache_read, t.cache_write, t.cache_write_1h, t.output, t.reasoning
    )?;
    for home in &report.codex_limits {
        writeln!(
            out,
            "Codex {} ({})",
            home.home,
            home.plan.as_deref().unwrap_or("unknown plan")
        )?;
        for window in &home.windows {
            writeln!(
                out,
                "  {}m: {}% at {} -> {}% at {}; resets {}",
                window.minutes,
                window.first.used_percent,
                window.first.at,
                window.last.used_percent,
                window.last.at,
                window.resets_at.as_deref().unwrap_or("unknown")
            )?;
        }
    }
    if !report.unpriced.is_empty() {
        writeln!(out, "Unpriced models: {}", report.unpriced.join(", "))?;
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<(), Error> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}\nValid tools: {}", Tool::accepted_names());
        return Ok(());
    }
    let options = Options::parse(args, crate::now_ms())?;
    let mut config = Config::load(&crate::config::path())?;
    if let Some(tools) = &options.tools {
        config.tools.clone_from(tools);
    }
    let sessions = Store::with_config(&config).snapshot()?;
    let (mut prices, stale) = crate::cached_prices();
    if stale && !options.offline {
        match crate::refresh_prices() {
            Ok(fresh) => prices = fresh,
            Err(error) => eprintln!("toktop: price refresh failed: {error}; using cached prices"),
        }
    }
    let source = if prices.is_empty() {
        "unavailable"
    } else {
        "litellm"
    }
    .to_owned();
    let fetched_at = prices
        .fetched_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|time| {
            time.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        })
        .or_else(|| {
            std::fs::metadata(crate::prices_cache_path())
                .ok()?
                .modified()
                .ok()
                .map(|time| {
                    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Millis, true)
                })
        });
    let overridden = options
        .price_file
        .as_deref()
        .map(|path| prices.apply_overrides(path))
        .transpose()?
        .unwrap_or_default();
    let source = if source == "unavailable" && !overridden.is_empty() {
        "price-file".to_owned()
    } else {
        source
    };
    let mut report = build(&sessions, &config, &prices, &options);
    report.prices = PriceInfo {
        source,
        fetched_at,
        overridden,
    };
    let mut out = io::stdout().lock();
    if options.json {
        serde_json::to_writer_pretty(&mut out, &report)?;
        writeln!(out)?;
    } else {
        table(&report, &mut out)?;
    }
    out.flush()?;
    Ok(())
}

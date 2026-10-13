use toktop::aggregate::{self, Period};
use toktop::{format, store::Store};

fn usage() -> ! {
    eprintln!("usage: toktop --print [today|week|7d|month|30d|all]");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--print") => print(args.get(1).map(String::as_str)),
        _ => usage(),
    }
}

fn print(period: Option<&str>) {
    let period = match period.unwrap_or("today") {
        "today" => Period::Today,
        "week" => Period::ThisWeek,
        "7d" => Period::Last7Days,
        "month" => Period::ThisMonth,
        "30d" => Period::Last30Days,
        "all" => Period::AllTime,
        _ => usage(),
    };
    let (mut prices, stale) = toktop::cached_prices();
    if stale {
        match toktop::refresh_prices() {
            Ok(fresh) => prices = fresh,
            Err(error) => eprintln!("toktop: price refresh failed: {error}"),
        }
    }
    let mut store = Store::default();
    store.sync_all();
    let summary = aggregate::summarize(&store, &prices, period, toktop::now_ms());

    let total = &summary.total;
    println!(
        "{}: {} tokens, {}, {} requests",
        period.label(),
        format::tokens(total.tokens.total()),
        format::cost(total.cost),
        total.requests
    );
    println!("\nBy tool");
    for (tool, usage) in &summary.by_tool {
        println!(
            "  {:<14} {:>8}  {:>9}",
            tool.label(),
            format::tokens(usage.tokens.total()),
            format::cost(usage.cost)
        );
    }
    for (title, rows) in [
        ("By model", &summary.by_model),
        ("By project", &summary.by_project),
    ] {
        println!("\n{title}");
        for row in rows.iter().take(10) {
            println!(
                "  {:<28} {:>8}  {:>9}",
                row.label,
                format::tokens(row.usage.tokens.total()),
                format::cost(row.usage.cost)
            );
        }
    }
    if let Some(limits) = &summary.codex_limits {
        println!(
            "\nCodex limits ({})",
            limits.plan.as_deref().unwrap_or("unknown plan")
        );
        for window in [&limits.primary, &limits.secondary].into_iter().flatten() {
            println!(
                "  {:>6} min window: {:.0}% used",
                window.window_minutes, window.used_percent
            );
        }
    }
    if !summary.unpriced.is_empty() {
        println!("\nUnpriced models: {}", summary.unpriced.join(", "));
    }
}

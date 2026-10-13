mod ui;

use toktop::aggregate::{self, Period};
use toktop::{format, store::Store};

fn usage() -> ! {
    eprintln!("usage: toktop [--background | --print [today|week|7d|month|30d|all]]");
    std::process::exit(2);
}

/// Uses a single glibc malloc arena. The parallel initial scan otherwise
/// leaves ~20 MB stranded in per-thread arenas for the life of the process.
fn limit_malloc_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        const M_ARENA_MAX: i32 = -8;
        unsafe extern "C" {
            fn mallopt(param: i32, value: i32) -> i32;
        }
        // SAFETY: called before any other thread exists.
        unsafe {
            mallopt(M_ARENA_MAX, 1);
        }
    }
}

fn main() -> std::process::ExitCode {
    limit_malloc_arenas();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("--background") => {
            let options = ui::Options {
                background: !args.is_empty(),
            };
            if ui::run(options) == gtk::glib::ExitCode::SUCCESS {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            }
        }
        Some("--print") => {
            print(args.get(1).map(String::as_str));
            std::process::ExitCode::SUCCESS
        }
        _ => usage(),
    }
}

fn print(period: Option<&str>) {
    let Some(period) = Period::from_key(period.unwrap_or("today")) else {
        usage();
    };
    let (mut prices, stale) = toktop::cached_prices();
    if stale {
        match toktop::refresh_prices() {
            Ok(fresh) => prices = fresh,
            Err(error) => eprintln!("toktop: price refresh failed: {error}"),
        }
    }
    let tools = toktop::config::Config::load(&toktop::config::path())
        .map(|config| config.tools)
        .unwrap_or_else(|_| toktop::model::Tool::ALL.to_vec());
    let mut store = Store::with_tools(tools);
    store.sync_all();
    let summary = aggregate::summarize(&store, &prices, period, toktop::now_ms());

    let cost = |value| {
        if summary.prices_available {
            format::cost(value)
        } else {
            "cost unavailable".to_owned()
        }
    };
    let total = &summary.total;
    println!(
        "{}: {} tokens, {}, {} requests",
        period.label(),
        format::tokens(total.tokens.total()),
        cost(total.cost),
        total.requests
    );
    println!("\nBy tool");
    for (tool, usage) in &summary.by_tool {
        println!(
            "  {:<14} {:>8}  {:>9}",
            tool.label(),
            format::tokens(usage.tokens.total()),
            cost(usage.cost)
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
                cost(row.usage.cost)
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

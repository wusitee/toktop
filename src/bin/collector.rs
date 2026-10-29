use std::io::{self, Write};
use toktop::{config::Config, model::Tool, remote::Snapshot, store::Store};

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if args.first().map(String::as_str) == Some("report") {
        return toktop::report::run(&args[1..]);
    }
    let usage = || {
        format!(
            "usage: toktop-collector --snapshot [--tools=tool,...]\n       toktop-collector {}\nValid tools: {}",
            toktop::report::USAGE,
            Tool::accepted_names()
        )
    };
    if matches!(args.first().map(String::as_str), Some("--help" | "-h")) && args.len() == 1 {
        println!("{}", usage());
        return Ok(());
    }
    if args.first().map(String::as_str) != Some("--snapshot") || args.len() > 2 {
        return Err(usage().into());
    }
    let mut config = Config::load(&toktop::config::path())?;
    // A standalone snapshot keeps its historical all-tools default; desktop
    // requests pass an explicit selection. Source settings come from this host.
    config.tools = if let Some(arg) = args.get(1) {
        Tool::parse_list(arg.strip_prefix("--tools=").ok_or_else(usage)?)?
    } else {
        Tool::ALL.to_vec()
    };
    let sessions = Store::with_config(&config).snapshot()?;
    let mut out = io::stdout().lock();
    serde_json::to_writer(
        &mut out,
        &Snapshot {
            version: 1,
            sessions,
        },
    )?;
    out.flush()?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("toktop-collector: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

use std::io::{self, Write};
use toktop::{model::Tool, remote::Snapshot, store::Store};

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("--snapshot") || args.len() > 2 {
        eprintln!("usage: toktop-collector --snapshot [--tools=tool,...]");
        return std::process::ExitCode::FAILURE;
    }
    let tools = if let Some(arg) = args.get(1) {
        let Some(list) = arg.strip_prefix("--tools=") else {
            eprintln!("toktop-collector: invalid tools argument");
            return std::process::ExitCode::FAILURE;
        };
        let parsed: Option<Vec<_>> = if list.is_empty() {
            Some(Vec::new())
        } else {
            list.split(',').map(Tool::from_key).collect()
        };
        let Some(tools) = parsed else {
            eprintln!("toktop-collector: unknown tool");
            return std::process::ExitCode::FAILURE;
        };
        tools
    } else {
        Tool::ALL.to_vec()
    };
    let result = Store::with_tools(tools).snapshot().and_then(|sessions| {
        serde_json::to_writer(
            io::stdout().lock(),
            &Snapshot {
                version: 1,
                sessions,
            },
        )?;
        io::stdout().lock().flush()
    });
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("toktop-collector: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

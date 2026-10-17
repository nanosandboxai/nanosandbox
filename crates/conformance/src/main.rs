use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use nanosb_conformance::scenario::Scenario;
use nanosb_conformance::ConformanceServer;
use nanosb_protocol::Capability;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        Some("--help") | Some("-h") | None => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn serve(args: &[String]) -> ExitCode {
    let mut scenario_path: Option<PathBuf> = None;
    let mut socket: Option<PathBuf> = None;
    let mut caps: Option<Vec<String>> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--scenario" => {
                i += 1;
                scenario_path = args.get(i).map(PathBuf::from);
            }
            "--socket" => {
                i += 1;
                socket = args.get(i).map(PathBuf::from);
            }
            "--capabilities" => {
                i += 1;
                caps = args
                    .get(i)
                    .map(|s| s.split(',').map(str::to_string).collect());
            }
            "--help" | "-h" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown flag: {other}");
                print_usage();
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }
    let Some(scenario_path) = scenario_path else {
        eprintln!("serve requires --scenario <file.json>");
        print_usage();
        return ExitCode::FAILURE;
    };
    let raw = match std::fs::read_to_string(&scenario_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("read scenario {}: {e}", scenario_path.display());
            return ExitCode::FAILURE;
        }
    };
    let scenario: Scenario = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("parse scenario {}: {e}", scenario_path.display());
            return ExitCode::FAILURE;
        }
    };
    let caps = caps.map(|names| {
        names
            .iter()
            .filter_map(|n| Capability::all().iter().find(|c| c.as_str() == n).copied())
            .collect()
    });
    let server = match ConformanceServer::start(scenario, socket, caps) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("start server: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("READY {}", server.path().display());
    let _ = std::io::stdout().flush();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

fn print_usage() {
    eprintln!(
        "nanosb-conformance — CI-safe protocol conformance harness\n\
         \n\
         USAGE:\n\
         \x20 nanosb-conformance serve --scenario <file.json> [--socket <path>] [--capabilities a,b,c]\n\
         \n\
         Prints `READY <socket-path>` on stdout once the server is listening.\n\
         \x20 --scenario      scenario JSON file to replay\n\
         \x20 --socket        socket path (default: a fresh temp path)\n\
         \x20 --capabilities  comma-separated capability override for the hello handshake"
    );
}

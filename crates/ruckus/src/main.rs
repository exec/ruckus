//! Command-line entry point for Ruckus.

use std::process::ExitCode;

#[cfg(any(feature = "gui", test))]
mod connection;
#[cfg(feature = "gui")]
mod desktop;
#[cfg(any(feature = "gui", test))]
mod model;
#[cfg(any(feature = "gui", test))]
mod presentation;
#[cfg(feature = "gui")]
mod theme;

const HELP: &str = "\
Ruckus — a Rust IRCv3 client based on Lurk

Usage: ruckus [--help | --version]

Options:
  -h, --help     Print help
  -V, --version  Print version

With no arguments, open the GPUI desktop client.
Build with the default `gui` feature to enable the desktop window.
";

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [] => return launch(),
        [arg] if arg == "--help" || arg == "-h" => print!("{HELP}"),
        [arg] if arg == "--version" || arg == "-V" => {
            println!("ruckus {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            // Do not echo arguments: future options may include credentials.
            eprintln!("error: unsupported arguments\n\nRun `ruckus --help` for usage.");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

#[cfg(feature = "gui")]
fn launch() -> ExitCode {
    desktop::run();
    ExitCode::SUCCESS
}

#[cfg(not(feature = "gui"))]
fn launch() -> ExitCode {
    eprintln!("error: desktop support is disabled; rebuild with --features gui");
    ExitCode::from(2)
}

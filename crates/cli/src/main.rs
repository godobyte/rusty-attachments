//! `ra` — Pure-Rust CLI for Deadline Cloud job attachment operations.

mod commands;
mod config;
mod output;
mod progress;

use clap::Parser;
use commands::Cli;

#[tokio::main]
async fn main() {
    let cli: Cli = Cli::parse();
    if let Err(e) = cli.run().await {
        output::print_error(&e);
        std::process::exit(1);
    }
}

//! CLI command definitions and dispatch.

pub mod attachment;
pub mod config_cmd;
pub mod manifest;

use clap::{Parser, Subcommand};

/// Errors from CLI operations.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Input validation error: {0}")]
    Validation(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Filesystem error: {0}")]
    Filesystem(String),

    #[error("Manifest error: {0}")]
    Manifest(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// `ra` — Pure-Rust CLI for Deadline Cloud job attachment operations.
#[derive(Parser)]
#[command(name = "ra", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manifest operations: snapshot, diff, upload, download.
    Manifest {
        #[command(subcommand)]
        action: manifest::ManifestAction,
    },
    /// Attachment operations: upload, download.
    Attachment {
        #[command(subcommand)]
        action: attachment::AttachmentAction,
    },
    /// Configuration management and benchmarking.
    Config {
        #[command(subcommand)]
        action: config_cmd::ConfigAction,
    },
}

impl Cli {
    /// Execute the parsed CLI command.
    pub async fn run(self) -> Result<(), CliError> {
        match self.command {
            Commands::Manifest { action } => action.run().await,
            Commands::Attachment { action } => action.run().await,
            Commands::Config { action } => action.run(),
        }
    }
}

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use rostra_core::id::RostraId;

/// Command line options for the Rostra CLI application
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Opts {
    /// Global options that apply to all commands
    #[command(flatten)]
    pub global: GlobalOpts,

    /// The specific command to execute
    #[command(subcommand)]
    pub cmd: OptsCmd,
}

/// Global options that apply across all commands
#[derive(Debug, Args)]
pub struct GlobalOpts {
    /// Temporary test flag (to be removed)
    #[arg(long)]
    pub test_delme: bool,
}

/// Available commands for the Rostra CLI
#[derive(Debug, Subcommand)]
pub enum OptsCmd {
    /// Generate a new Rostra ID
    GenId,
    /// Start the Rostra server
    Serve {
        /// Path to the secret file for authentication
        #[clap(long)]
        secret_file: Option<PathBuf>,
    },

    /// Development and debugging commands
    #[command(subcommand)]
    Dev(DevCmd),

    /// Post a message
    Post {
        /// Message body to post
        #[clap(long)]
        body: String,

        /// Path to the secret file for authentication
        #[clap(long)]
        secret_file: PathBuf,
    },
}

/// Development and debugging commands
#[derive(Debug, Subcommand)]
pub enum DevCmd {
    /// Resolve a Rostra ID
    ResolveId {
        /// The Rostra ID to resolve
        id: RostraId,
    },
    /// Ping a specific Rostra ID
    Ping {
        /// The target Rostra ID to ping
        id: RostraId,
        /// Sequence number to start from
        #[clap(long, default_value = "0")]
        seq: u64,
        /// Number of pings to send
        #[clap(long, default_value = "1")]
        count: u64,
        /// Whether to establish only one connection
        #[clap(long)]
        connect_once: bool,
    },
    /// Run tests
    Test,
}

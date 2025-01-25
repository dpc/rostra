use std::path::{Path, PathBuf};
use std::sync::LazyLock;

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
    #[arg(env = "ROSTRA_DATA_DIR", long)]
    pub data_dir: Option<PathBuf>,
}

static PROJECTS_DIR: LazyLock<directories::ProjectDirs> = LazyLock::new(|| {
    directories::ProjectDirs::from("org", "Rostra", "rostra")
        .expect("Unable to determine project's dir")
});

impl GlobalOpts {
    pub fn data_dir(&self) -> &Path {
        self.data_dir.as_deref().unwrap_or_else(|| {
            PROJECTS_DIR
                .state_dir()
                .unwrap_or_else(|| PROJECTS_DIR.data_local_dir())
        })
    }
}

/// Available commands for the Rostra CLI
#[derive(Debug, Subcommand)]
pub enum OptsCmd {
    /// Generate a new Rostra ID
    GenId,
    /// Start the Rostra server
    Serve {
        /// Path to the secret file for authentication
        #[arg(long)]
        secret_file: Option<PathBuf>,
    },
    WebUi(WebUiOpts),

    /// Development and debugging commands
    #[command(subcommand)]
    Dev(DevCmd),

    /// Post a message
    Post {
        /// Message body to post
        #[arg(long)]
        body: String,

        /// Path to the secret file for authentication
        #[arg(long)]
        secret_file: PathBuf,
    },
}

/// Global options that apply across all commands
#[derive(Debug, Args)]
pub struct WebUiOpts {
    /// Path to the secret file for authentication
    #[arg(long)]
    pub secret_file: Option<PathBuf>,

    #[arg(long)]
    pub skip_xdg_open: bool,

    /// Listen address
    #[arg(long, short, default_value = "[::1]:0", env = "ROSTRA_LISTEN")]
    pub listen: String,

    /// Set SO_REUSEPORT
    #[arg(long, env = "ROSTRA_REUSEPORT")]
    pub reuseport: bool,

    /// Cors origin settings
    #[arg(long, env = "ROSTRA_CORS_ORIGIN")]
    pub cors_origin: Option<String>,

    /// Root directory of the assets dir
    #[arg(long, env = "ROSTRA_ASSETS_DIR")]
    pub assets_dir: Option<PathBuf>,
}

pub fn make_web_opts(data_dir: &Path, opts: &WebUiOpts) -> rostra_web_ui::Opts {
    rostra_web_ui::Opts::new(
        opts.listen.clone(),
        opts.cors_origin.clone(),
        opts.assets_dir.clone(),
        opts.reuseport,
        data_dir.to_owned(),
        opts.secret_file.clone(),
    )
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
        #[arg(long, default_value = "0")]
        seq: u64,
        /// Number of pings to send
        #[arg(long, default_value = "1")]
        count: u64,
        /// Whether to establish only one connection
        #[arg(long)]
        connect_once: bool,
    },
    /// Run tests
    Test,
}

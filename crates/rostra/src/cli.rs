use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use clap::{Args, Parser, Subcommand};
use rostra_core::event::PersonaId;
use rostra_core::id::RostraId;
use rostra_util_bind_addr::BindAddr;

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
        /// Default profile to use for users who haven't logged in yet
        /// (read-only mode)
        #[arg(long, group = "id", env = "ROSTRA_ID")]
        id: Option<RostraId>,

        /// Path to the secret file for authentication
        #[arg(long, group = "id")]
        secret_file: Option<PathBuf>,
    },
    /// Start web-ui
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

        #[arg(long)]
        persona_id: Option<PersonaId>,
    },
}

#[derive(Debug, Args)]
pub struct WebUiOpts {
    /// Path to the secret file for authentication
    #[arg(long)]
    pub secret_file: Option<PathBuf>,

    /// Default profile to use for users who haven't logged in yet (read-only
    /// mode)
    #[arg(long)]
    pub default_profile: Option<RostraId>,

    /// Maximum number of clients to keep loaded at once
    #[arg(long, default_value = "10")]
    pub max_clients: usize,

    #[arg(long)]
    pub skip_xdg_open: bool,

    /// Listen address
    ///
    /// Either a socket addr (TCP) or a path (Unix socket)
    #[arg(long, short, default_value = "[::1]:3377", env = "ROSTRA_LISTEN")]
    pub listen: BindAddr,

    /// Set SO_REUSEPORT
    #[arg(long, env = "ROSTRA_REUSEPORT", default_value = "true")]
    pub reuseport: bool,

    /// Public origin URL (e.g. "rostra.me" or "https://rostra.me").
    /// Used for CORS and absolute URLs in meta tags.
    /// If no scheme is provided, https:// is assumed.
    #[arg(
        long,
        env = "ROSTRA_ORIGIN",
        alias = "cors-origin",
        hide_long_help = false
    )]
    pub origin: Option<String>,

    /// Enable public mode - allows direct IP connections (exposes your IP
    /// address). By default, Rostra uses relay-only mode for privacy.
    #[arg(long, env = "ROSTRA_PUBLIC")]
    pub public: bool,

    /// Root directory of the assets dir
    #[arg(long, env = "ROSTRA_ASSETS_DIR")]
    pub assets_dir: Option<PathBuf>,

    /// Custom URL to redirect unauthenticated users from "/" instead of showing
    /// welcome page
    #[arg(long, env = "ROSTRA_WELCOME_REDIRECT")]
    pub welcome_redirect: Option<String>,
}

pub fn make_web_opts(data_dir: &Path, opts: &WebUiOpts) -> rostra_web_ui::Opts {
    rostra_web_ui::Opts::new(
        opts.listen.clone(),
        opts.origin.clone(),
        opts.assets_dir.clone(),
        opts.reuseport,
        data_dir.to_owned(),
        opts.default_profile,
        opts.max_clients,
        opts.welcome_redirect.clone(),
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
    /// Dump database
    DbDump {
        #[arg(long)]
        table: String,
        #[arg(long)]
        rostra_id: RostraId,
    },
}

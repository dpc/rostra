use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use rostra_core::id::RostraId;

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Opts {
    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(subcommand)]
    pub cmd: OptsCmd,
}

#[derive(Debug, Args)]
pub struct GlobalOpts {
    #[arg(long)]
    pub test_delme: bool,
}

#[derive(Debug, Subcommand)]
pub enum OptsCmd {
    GenId,
    Serve,

    #[command(subcommand)]
    Dev(DevCmd),

    Post {
        #[clap(long)]
        body: String,

        #[clap(long)]
        secret_file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum DevCmd {
    ResolveId {
        id: RostraId,
    },
    Ping {
        id: RostraId,
        #[clap(long, default_value = "0")]
        seq: u64,
        #[clap(long, default_value = "1")]
        count: u64,
        #[clap(long)]
        connect_once: bool,
    },
    Test,
}

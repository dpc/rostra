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
    pub test: bool,
}

#[derive(Debug, Subcommand)]
pub enum OptsCmd {
    Serve,

    #[command(subcommand)]
    Dev(DevCmd),
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

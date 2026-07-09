use clap::{Parser, Subcommand};
use scx_loader::SchedMode;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Parser, Debug)]
#[group(required = true)]
pub struct StartArgs {
    #[arg(short, long, help = "Scheduler to start", required = true)]
    pub sched: String,
    #[arg(
        short,
        long,
        value_enum,
        default_value = "auto",
        conflicts_with = "args",
        help = "Mode to start in"
    )]
    pub mode: Option<SchedMode>,
    #[arg(
        short,
        long,
        value_delimiter(','),
        requires = "sched",
        conflicts_with = "mode",
        help = "Arguments to run scheduler with"
    )]
    pub args: Option<Vec<String>>,
}

#[derive(Parser, Debug)]
#[group(required = true)]
pub struct SwitchArgs {
    #[arg(short, long, help = "Scheduler to switch to")]
    pub sched: Option<String>,
    #[arg(
        short,
        long,
        value_enum,
        conflicts_with = "args",
        help = "Mode to switch to"
    )]
    pub mode: Option<SchedMode>,
    #[arg(
        short,
        long,
        value_delimiter(','),
        requires = "sched",
        conflicts_with = "mode",
        help = "Arguments to run scheduler with"
    )]
    pub args: Option<Vec<String>>,
}

#[derive(Parser, Debug)]
pub struct HoldArgs {
    #[arg(short, long, help = "Scheduler to hold", required = true)]
    pub sched: String,
    #[arg(
        short,
        long,
        value_enum,
        default_value = "auto",
        help = "Mode to hold in"
    )]
    pub mode: SchedMode,
    #[arg(
        short,
        long,
        help = "Human-readable reason for the hold",
        required = true
    )]
    pub reason: String,
    #[arg(
        short,
        long,
        help = "Reverse-DNS application identifier (e.g. org.gnome.Games)",
        required = true
    )]
    pub app_id: String,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(about = "Get the info on the running scheduler")]
    Get,
    #[command(about = "List all supported schedulers")]
    List,
    #[command(about = "Start a scheduler in a mode or with arguments")]
    Start {
        #[clap(flatten)]
        args: StartArgs,
    },
    #[command(about = "Switch schedulers or modes, optionally with arguments")]
    Switch {
        #[clap(flatten)]
        args: SwitchArgs,
    },
    #[command(about = "Stop the current scheduler")]
    Stop,
    #[command(about = "Restart the current scheduler with original configuration")]
    Restart,
    #[command(about = "Restore the default scheduler from configuration")]
    Restore,
    #[command(about = "Place a scheduler hold and print the returned cookie")]
    Hold {
        #[clap(flatten)]
        args: HoldArgs,
    },
    #[command(about = "Release a previously acquired scheduler hold by cookie")]
    Release {
        #[arg(help = "Cookie returned by the 'hold' command")]
        cookie: u32,
    },
    #[command(about = "List all currently active scheduler holds")]
    Holds,
}

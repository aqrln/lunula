#![feature(option_into_flat_iter)]

use argh::FromArgs;
use tracing_subscriber::{EnvFilter, fmt::time::LocalTime, util::SubscriberInitExt};

mod workspace;

/// Lunula development tasks
#[derive(FromArgs)]
struct Cli {
    #[argh(subcommand)]
    command: Command,
}

impl Cli {
    fn run(self) -> anyhow::Result<()> {
        match self.command {
            Command::Workspace(workspace) => workspace.run(),
        }
    }
}

#[derive(FromArgs)]
#[argh(subcommand)]
enum Command {
    Workspace(workspace::WorkspaceCommand),
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(tracing::Level::INFO.into())
                .from_env()?,
        )
        .pretty()
        .with_timer(LocalTime::new(time::macros::format_description!(
            "[hour]:[minute]:[second]"
        )))
        .finish()
        .init();
    argh::from_env::<Cli>().run()
}

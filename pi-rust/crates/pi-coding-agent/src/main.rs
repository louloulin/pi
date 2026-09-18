//! `pi` binary entry point — Stage 0 stub.

use clap::Parser;
use pi_coding_agent::cli::{Cli, Command};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Version) => {
            println!("pi (rust) {}", env!("CARGO_PKG_VERSION"));
        }
        Some(Command::Install { .. })
        | Some(Command::Remove { .. })
        | Some(Command::List)
        | Some(Command::UpdateModels) => {
            anyhow::bail!("pi (rust) is at Stage 0 scaffold — subcommand support lands in Stage 4");
        }
        None => {
            // Interactive / print / rpc mode — Stage 4.
            if cli.rpc {
                anyhow::bail!("pi --rpc lands in Stage 4");
            }
            if cli.print {
                anyhow::bail!("pi --print lands in Stage 4");
            }
            println!(
                "pi (rust) {} scaffold running. Interactive mode lands in Stage 4.",
                env!("CARGO_PKG_VERSION")
            );
        }
    }
    Ok(())
}

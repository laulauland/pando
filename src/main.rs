use anyhow::Result;
use clap::{Parser, Subcommand};
use pando::{
    backend::PlatformCowBackend,
    home::pando_home,
    lifecycle::{create_workspace, destroy_workspace, list_workspaces},
};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "pando", version, about = "Lightweight workspace lifecycle CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a workspace from a source directory.
    Create {
        name: String,
        /// Source directory to copy. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        from: PathBuf,
    },
    /// List known workspaces.
    List,
    /// Destroy a workspace and its lifecycle state.
    Destroy { name: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = pando_home()?;
    let backend = PlatformCowBackend::default();

    match cli.command {
        Command::Create { name, from } => {
            let workspace_path = create_workspace(&home, &backend, &name, &from)?;
            println!("{}", workspace_path.display());
        }
        Command::List => {
            for metadata in list_workspaces(&home)? {
                println!("{}\t{}", metadata.name, metadata.workspace_path.display());
            }
        }
        Command::Destroy { name } => {
            destroy_workspace(&home, &backend, &name)?;
        }
    }

    Ok(())
}

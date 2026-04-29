use crate::{
    backend::PlatformCowBackend,
    home::pando_home,
    lifecycle::{create_workspace, destroy_workspace, list_workspaces},
};
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "pando", version, about = "Lightweight workspace lifecycle CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a V1 workspace from a source directory.
    Create {
        name: String,
        /// Source directory path to copy/clone. Defaults to the current directory.
        #[arg(long, default_value = ".", value_name = "PATH")]
        from: PathBuf,
    },
    /// List known workspaces.
    List,
    /// Destroy a workspace and its lifecycle state.
    Destroy {
        name: String,
        /// Accepted for the future V2 jj backend; currently a V1 no-op.
        #[arg(long)]
        keep_jj_workspace: bool,
    },
}

pub fn run() -> Result<()> {
    run_from(Cli::parse())
}

fn run_from(cli: Cli) -> Result<()> {
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
        Command::Destroy {
            name,
            keep_jj_workspace: _,
        } => {
            destroy_workspace(&home, &backend, &name)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn create_accepts_name_and_optional_from() {
        let cli =
            Cli::try_parse_from(["pando", "create", "demo", "--from", "/tmp/source"]).unwrap();

        match cli.command {
            Command::Create { name, from } => {
                assert_eq!(name, "demo");
                assert_eq!(from, PathBuf::from("/tmp/source"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn create_defaults_from_to_current_directory() {
        let cli = Cli::try_parse_from(["pando", "create", "demo"]).unwrap();

        match cli.command {
            Command::Create { from, .. } => assert_eq!(from, PathBuf::from(".")),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn destroy_accepts_keep_jj_workspace_as_noop_flag() {
        let cli = Cli::try_parse_from(["pando", "destroy", "demo", "--keep-jj-workspace"]).unwrap();

        match cli.command {
            Command::Destroy {
                name,
                keep_jj_workspace,
            } => {
                assert_eq!(name, "demo");
                assert!(keep_jj_workspace);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}

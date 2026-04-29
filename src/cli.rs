use crate::{
    backend::PlatformCowBackend,
    home::pando_home,
    lifecycle::{create_workspace, destroy_workspace, list_workspaces},
};
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::env;

#[derive(Debug, Parser)]
#[command(name = "pando", version, about = "Lightweight workspace lifecycle CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a V1 workspace from the current directory.
    Create {
        name: String,
        /// jj revset to base the new workspace on. Ignored outside jj repositories.
        #[arg(long, value_name = "REVSET")]
        from: Option<String>,
    },
    /// List known workspaces.
    List,
    /// Destroy a workspace and its lifecycle state.
    Destroy {
        name: String,
        /// Destroy Pando state without forgetting the native jj workspace.
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
            let source = env::current_dir()?;
            let workspace_path =
                create_workspace(&home, &backend, &name, &source, from.as_deref())?;
            println!("{}", workspace_path.display());
        }
        Command::List => {
            for metadata in list_workspaces(&home)? {
                println!("{}\t{}", metadata.name, metadata.workspace_path.display());
            }
        }
        Command::Destroy {
            name,
            keep_jj_workspace,
        } => {
            destroy_workspace(&home, &backend, &name, keep_jj_workspace)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser;

    #[test]
    fn create_accepts_name_and_optional_from_revset() {
        let cli = Cli::try_parse_from(["pando", "create", "demo", "--from", "@-"]).unwrap();

        match cli.command {
            Command::Create { name, from } => {
                assert_eq!(name, "demo");
                assert_eq!(from.as_deref(), Some("@-"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn create_defaults_from_to_none() {
        let cli = Cli::try_parse_from(["pando", "create", "demo"]).unwrap();

        match cli.command {
            Command::Create { from, .. } => assert_eq!(from, None),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn destroy_accepts_keep_jj_workspace_flag() {
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

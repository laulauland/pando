use crate::{
    backend::PlatformCowBackend,
    home::pando_home,
    lifecycle::{create_workspace, destroy_workspace, list_workspaces},
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use std::env;

#[derive(Debug, Parser)]
#[command(
    name = "pando",
    disable_version_flag = true,
    about = "Create and manage isolated Pando workspaces"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a Pando workspace from the current directory.
    Create {
        name: String,
        /// Select the jj base revision for the new workspace. Ignored outside jj repositories.
        #[arg(long, value_name = "REVSET")]
        from: Option<String>,
    },
    /// List Pando workspaces.
    List,
    /// Destroy a Pando workspace and its state.
    Destroy {
        name: String,
        /// Keep the jj workspace while removing Pando state.
        #[arg(long)]
        keep_jj_workspace: bool,
    },
}

pub fn run() -> Result<()> {
    run_from(Cli::parse())
}

fn short_commit(commit: &str) -> String {
    commit.chars().take(12).collect()
}

fn format_age(created_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let age = now.signed_duration_since(created_at);
    if age.num_days() > 0 {
        format!("{}d", age.num_days())
    } else if age.num_hours() > 0 {
        format!("{}h", age.num_hours())
    } else if age.num_minutes() > 0 {
        format!("{}m", age.num_minutes())
    } else {
        format!("{}s", age.num_seconds().max(0))
    }
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
            println!("NAME\tAGE\tBASE\tJJ");
            for metadata in list_workspaces(&home)? {
                let age = format_age(metadata.created_at, Utc::now());
                let base = metadata
                    .jj
                    .as_ref()
                    .and_then(|jj| jj.base_commit.as_deref())
                    .map(short_commit)
                    .unwrap_or_else(|| "-".to_owned());
                let jj = metadata
                    .jj
                    .as_ref()
                    .and_then(|jj| jj.workspace_name.as_deref())
                    .unwrap_or("-");
                println!("{}\t{}\t{}\t{}", metadata.name, age, base, jj);
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
    use super::{format_age, short_commit, Cli, Command};
    use chrono::{Duration, Utc};
    use clap::{CommandFactory, Parser};

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
    fn list_helpers_use_product_display_values() {
        assert_eq!(short_commit("1234567890abcdef"), "1234567890ab");

        let now = Utc::now();
        assert_eq!(format_age(now - Duration::seconds(3), now), "3s");
        assert_eq!(format_age(now - Duration::minutes(2), now), "2m");
        assert_eq!(format_age(now - Duration::hours(4), now), "4h");
        assert_eq!(format_age(now - Duration::days(5), now), "5d");
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

    #[test]
    fn help_avoids_version_and_implementation_details() {
        let mut command = Cli::command();
        let help = command.render_help().to_string();

        assert!(!help.contains("--version"));
        assert!(!help.contains("implementation"));
        assert!(!help.contains("native"));
    }
}

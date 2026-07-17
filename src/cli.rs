#[cfg(not(feature = "microvm-boxlite"))]
use crate::lifecycle::{create_workspace, destroy_workspace, list_workspaces};
use crate::{
    backend::PlatformCowBackend,
    home::{legacy_pando_home, pando_home, state_dir},
    metadata::JjMetadata,
    migration::migrate_legacy_home_if_needed,
};
#[cfg(any(test, not(feature = "microvm-boxlite")))]
use crate::{
    metadata::{has_runtime_transaction, read_metadata},
    naming::validate_name,
};
#[cfg(not(feature = "microvm-boxlite"))]
use anyhow::bail;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use serde::Serialize;
use std::{
    env,
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitStatus},
};

#[derive(Debug, Parser)]
#[command(version, about = "Create and manage isolated Pando workspaces")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a Pando workspace from the current directory.
    Create {
        /// Pando workspace name.
        name: String,
        /// Select the jj base revision for the new workspace. Ignored outside jj repositories.
        #[arg(long, value_name = "REVSET")]
        from: Option<String>,
        /// Run the workspace in an optional execution environment.
        #[arg(long, value_enum)]
        runtime: Option<RuntimeChoice>,
        /// OCI image for the execution environment.
        #[arg(long, requires = "runtime")]
        image: Option<String>,
    },
    /// List Pando workspaces.
    List,
    /// Print workspace facts.
    #[command(visible_alias = "get")]
    Info {
        /// Pando workspace name.
        name: String,
        /// Print workspace facts as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Open a shell in a workspace.
    Cd {
        /// Pando workspace name.
        name: String,
        /// Print the workspace path instead of opening a shell.
        #[arg(long)]
        print: bool,
    },
    /// Execute a command in a workspace runtime.
    Exec {
        /// Pando workspace name.
        name: String,
        /// Command and arguments to execute without shell interpretation.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    /// Open an interactive shell in a workspace runtime.
    Shell {
        /// Pando workspace name.
        name: String,
    },
    /// Stop a workspace runtime.
    Stop {
        /// Pando workspace name.
        name: String,
    },
    /// Remove a Pando workspace and its state.
    #[command(visible_alias = "rm", alias = "destroy")]
    Remove {
        /// Pando workspace name.
        name: String,
        /// Keep the jj workspace while removing Pando state.
        #[arg(long)]
        keep_jj_workspace: bool,
    },
    /// Print a shell completion script to stdout.
    Completions {
        /// Shell to generate completions for.
        shell: Shell,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum RuntimeChoice {
    Boxlite,
}

pub fn run() -> Result<()> {
    let binary_name = invoked_binary_name(env::args_os().next().as_deref());
    let command_name = command_name_for_binary(&binary_name);
    let matches = Cli::command().name(command_name).get_matches();
    let cli = Cli::from_arg_matches(&matches)?;

    run_from(cli, command_name)
}

fn invoked_binary_name(arg0: Option<&OsStr>) -> String {
    arg0.map(Path::new)
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("pando")
        .to_owned()
}

fn command_name_for_binary(binary_name: &str) -> &'static str {
    if binary_name == "pd" {
        "pd"
    } else {
        "pando"
    }
}

fn base_revision_for_list(canonical_root: &Path, jj: &crate::metadata::JjMetadata) -> String {
    if let Some(revision) = jj.base_revision.as_deref() {
        return revision.to_owned();
    }

    if let Some(commit) = jj.base_commit.as_deref() {
        return crate::jj::lookup_base_revision(canonical_root, commit)
            .ok()
            .flatten()
            .unwrap_or_else(|| "-".to_owned());
    }

    "-".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListRow {
    name: String,
    age: String,
    base: String,
    jj: String,
}

fn format_workspace_list(rows: &[ListRow]) -> String {
    format_table(
        ["NAME", "AGE", "BASE", "JJ"],
        rows.iter().map(|row| {
            [
                row.name.as_str(),
                row.age.as_str(),
                row.base.as_str(),
                row.jj.as_str(),
            ]
        }),
    )
}

fn format_table<'a, const N: usize>(
    headers: [&'a str; N],
    rows: impl IntoIterator<Item = [&'a str; N]>,
) -> String {
    let rows = rows.into_iter().collect::<Vec<_>>();
    let widths = std::array::from_fn::<_, N, _>(|column| {
        rows.iter()
            .map(|row| row[column].len())
            .max()
            .unwrap_or(0)
            .max(headers[column].len())
    });

    let mut output = String::new();
    write_table_row(&mut output, headers, widths);
    for row in rows {
        write_table_row(&mut output, row, widths);
    }

    output
}

fn write_table_row<const N: usize>(output: &mut String, row: [&str; N], widths: [usize; N]) {
    for (column, value) in row.iter().enumerate() {
        if column > 0 {
            output.push_str("  ");
        }

        if column == N - 1 {
            output.push_str(value);
        } else if column == 1 {
            output.push_str(&format!("{:>width$}", value, width = widths[column]));
        } else {
            output.push_str(&format!("{:<width$}", value, width = widths[column]));
        }
    }
    output.push('\n');
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct WorkspaceInfo {
    name: String,
    state_dir: PathBuf,
    workspace_path: PathBuf,
    canonical_root: PathBuf,
    created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    jj: Option<WorkspaceJjInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<WorkspaceRuntimeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct WorkspaceRuntimeInfo {
    kind: &'static str,
    provider_id: crate::runtime::RuntimeIdentity,
    image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<crate::runtime::RuntimeStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct WorkspaceJjInfo {
    workspace_name: Option<String>,
    base_commit: Option<String>,
    base_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_path: Option<PathBuf>,
}

#[cfg(any(test, not(feature = "microvm-boxlite")))]
fn workspace_info(home: &Path, name: &str) -> Result<WorkspaceInfo> {
    validate_name(name)?;
    if has_runtime_transaction(home, name)? {
        anyhow::bail!("workspace creation is incomplete and requires runtime recovery: {name}");
    }
    let state_dir = state_dir(home, name);
    let metadata =
        read_metadata(&state_dir).with_context(|| format!("workspace not found: {name}"))?;

    Ok(workspace_info_from_metadata(state_dir, metadata))
}

fn workspace_info_from_metadata(
    state_dir: PathBuf,
    metadata: crate::metadata::Metadata,
) -> WorkspaceInfo {
    let runtime = metadata
        .runtime
        .clone()
        .map(|runtime| WorkspaceRuntimeInfo {
            kind: "boxlite",
            provider_id: runtime.identity,
            image: runtime.image,
            state: None,
        });
    WorkspaceInfo {
        name: metadata.name,
        state_dir,
        workspace_path: metadata.workspace_path,
        canonical_root: metadata.canonical_root.clone(),
        created_at: metadata.created_at,
        jj: metadata
            .jj
            .map(|jj| workspace_jj_info(jj, &metadata.canonical_root)),
        runtime,
    }
}

fn workspace_jj_info(jj: JjMetadata, canonical_root: &Path) -> WorkspaceJjInfo {
    let repo_path = canonical_root.join(".jj/repo");
    WorkspaceJjInfo {
        workspace_name: jj.workspace_name,
        base_commit: jj.base_commit,
        base_revision: jj.base_revision,
        repo_path: repo_path.is_dir().then_some(repo_path),
    }
}

fn format_workspace_info_table(info: &WorkspaceInfo) -> String {
    let state_dir = info.state_dir.to_string_lossy();
    let workspace_path = info.workspace_path.to_string_lossy();
    let canonical_root = info.canonical_root.to_string_lossy();
    let created_at = info.created_at.to_rfc3339();
    let jj_workspace = info
        .jj
        .as_ref()
        .and_then(|jj| jj.workspace_name.as_deref())
        .unwrap_or("-");
    let jj_base = info
        .jj
        .as_ref()
        .and_then(|jj| jj.base_revision.as_deref())
        .unwrap_or("-");
    let runtime_kind = info
        .runtime
        .as_ref()
        .map(|runtime| runtime.kind)
        .unwrap_or("-");
    let runtime_id = info
        .runtime
        .as_ref()
        .map(|runtime| runtime.provider_id.as_str())
        .unwrap_or("-");
    let runtime_image = info
        .runtime
        .as_ref()
        .map(|runtime| runtime.image.as_str())
        .unwrap_or("-");
    let runtime_state = info
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.state)
        .map(|state| format!("{state:?}").to_ascii_lowercase())
        .unwrap_or_else(|| "-".to_owned());

    format_table(
        ["FIELD", "VALUE"],
        [
            ["name", info.name.as_str()],
            ["workspace", workspace_path.as_ref()],
            ["state", state_dir.as_ref()],
            ["canonical", canonical_root.as_ref()],
            ["created", created_at.as_str()],
            ["jj", jj_workspace],
            ["base", jj_base],
            ["runtime", runtime_kind],
            ["runtime-id", runtime_id],
            ["image", runtime_image],
            ["runtime-state", runtime_state.as_str()],
        ],
    )
}

fn open_shell_in_directory(directory: &Path) -> Result<ExitStatus> {
    let shell = env::var_os("SHELL").unwrap_or_else(|| OsStr::new("sh").to_owned());
    ProcessCommand::new(shell)
        .current_dir(directory)
        .status()
        .with_context(|| format!("could not open shell in {}", directory.display()))
}

fn prepare_home() -> Result<(PathBuf, PlatformCowBackend)> {
    let home = pando_home()?;
    let legacy_home = legacy_pando_home()?;
    let backend = PlatformCowBackend::default();
    migrate_legacy_home_if_needed(&legacy_home, &home, &backend)?;
    Ok((home, backend))
}

fn run_from(cli: Cli, binary_name: &'static str) -> Result<()> {
    match cli.command {
        Command::Create {
            name,
            from,
            runtime,
            image,
        } => {
            let (home, backend) = prepare_home()?;
            let source = env::current_dir()?;
            let workspace_path = match runtime {
                None => {
                    #[cfg(feature = "microvm-boxlite")]
                    {
                        run_async(crate::lifecycle::create_workspace_reconciled(
                            &home,
                            &backend,
                            &name,
                            &source,
                            from.as_deref(),
                        ))?
                    }
                    #[cfg(not(feature = "microvm-boxlite"))]
                    {
                        create_workspace(&home, &backend, &name, &source, from.as_deref())?
                    }
                }
                Some(RuntimeChoice::Boxlite) => {
                    #[cfg(feature = "microvm-boxlite")]
                    {
                        run_async(crate::lifecycle::create_workspace_with_runtime(
                            &home,
                            &backend,
                            &name,
                            &source,
                            from.as_deref(),
                            image.unwrap_or_else(|| "alpine:3.22".to_owned()),
                        ))?
                    }
                    #[cfg(not(feature = "microvm-boxlite"))]
                    {
                        let _ = image;
                        bail!("BoxLite support is not enabled in this Pando build")
                    }
                }
            };
            println!("{}", workspace_path.display());
        }
        Command::List => {
            let (home, backend) = prepare_home()?;
            #[cfg(not(feature = "microvm-boxlite"))]
            let _ = &backend;
            #[cfg(feature = "microvm-boxlite")]
            let workspaces = run_async(crate::lifecycle::list_workspaces_reconciled(
                &home, &backend,
            ))?;
            #[cfg(not(feature = "microvm-boxlite"))]
            let workspaces = list_workspaces(&home)?;
            let rows = workspaces
                .into_iter()
                .map(|metadata| {
                    let age = format_age(metadata.created_at, Utc::now());
                    let base = metadata
                        .jj
                        .as_ref()
                        .map(|jj| base_revision_for_list(&metadata.canonical_root, jj))
                        .unwrap_or_else(|| "-".to_owned());
                    let jj = metadata
                        .jj
                        .as_ref()
                        .and_then(|jj| jj.workspace_name.as_deref())
                        .unwrap_or("-")
                        .to_owned();
                    ListRow {
                        name: metadata.name,
                        age,
                        base,
                        jj,
                    }
                })
                .collect::<Vec<_>>();
            print!("{}", format_workspace_list(&rows));
        }
        Command::Info { name, json } => {
            let (home, _) = prepare_home()?;
            #[cfg(feature = "microvm-boxlite")]
            let info = {
                let (metadata, observed) =
                    run_async(crate::lifecycle::inspect_workspace_runtime(&home, &name))?;
                let mut info = workspace_info_from_metadata(state_dir(&home, &name), metadata);
                if let (Some(runtime), Some(observed)) = (info.runtime.as_mut(), observed) {
                    runtime.state = Some(observed.status);
                }
                info
            };
            #[cfg(not(feature = "microvm-boxlite"))]
            let info = workspace_info(&home, &name)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                print!("{}", format_workspace_info_table(&info));
            }
        }
        Command::Cd { name, print } => {
            let (home, backend) = prepare_home()?;
            #[cfg(not(feature = "microvm-boxlite"))]
            let _ = &backend;
            #[cfg(feature = "microvm-boxlite")]
            let info = workspace_info_from_metadata(
                state_dir(&home, &name),
                run_async(crate::lifecycle::read_workspace_reconciled(
                    &home, &backend, &name,
                ))?,
            );
            #[cfg(not(feature = "microvm-boxlite"))]
            let info = workspace_info(&home, &name)?;
            if print {
                println!("{}", info.workspace_path.display());
            } else {
                let status = open_shell_in_directory(&info.workspace_path)?;
                if let Some(code) = status.code() {
                    std::process::exit(code);
                }
            }
        }
        Command::Exec { name, arguments } => {
            #[cfg(feature = "microvm-boxlite")]
            {
                let (home, _) = prepare_home()?;
                let code = run_async(crate::lifecycle::execute_in_workspace(
                    &home, &name, arguments, false,
                ))?;
                if code != 0 {
                    std::process::exit(code);
                }
            }
            #[cfg(not(feature = "microvm-boxlite"))]
            {
                let _ = (name, arguments);
                bail!("BoxLite support is not enabled in this Pando build");
            }
        }
        Command::Shell { name } => {
            #[cfg(feature = "microvm-boxlite")]
            {
                let (home, _) = prepare_home()?;
                let code = run_async(crate::lifecycle::execute_in_workspace(
                    &home,
                    &name,
                    vec!["/bin/sh".to_owned()],
                    true,
                ))?;
                if code != 0 {
                    std::process::exit(code);
                }
            }
            #[cfg(not(feature = "microvm-boxlite"))]
            {
                let _ = name;
                bail!("BoxLite support is not enabled in this Pando build");
            }
        }
        Command::Stop { name } => {
            #[cfg(feature = "microvm-boxlite")]
            {
                let (home, _) = prepare_home()?;
                run_async(crate::lifecycle::stop_workspace_runtime(&home, &name))?;
            }
            #[cfg(not(feature = "microvm-boxlite"))]
            {
                let _ = name;
                bail!("BoxLite support is not enabled in this Pando build")
            }
        }
        Command::Remove {
            name,
            keep_jj_workspace,
        } => {
            crate::naming::validate_name(&name)?;
            let (home, backend) = prepare_home()?;
            #[cfg(feature = "microvm-boxlite")]
            {
                run_async(crate::lifecycle::destroy_workspace_with_runtime(
                    &home,
                    &backend,
                    &name,
                    keep_jj_workspace,
                ))?;
            }
            #[cfg(not(feature = "microvm-boxlite"))]
            {
                let has_runtime = read_metadata(&state_dir(&home, &name))
                    .ok()
                    .and_then(|metadata| metadata.runtime)
                    .is_some();
                if has_runtime {
                    bail!("BoxLite support is not enabled in this Pando build");
                } else {
                    destroy_workspace(&home, &backend, &name, keep_jj_workspace)?;
                }
            }
        }
        Command::Completions { shell } => {
            let mut command = Cli::command().name(binary_name);
            generate(shell, &mut command, binary_name, &mut io::stdout());
        }
    }

    Ok(())
}

#[cfg(feature = "microvm-boxlite")]
fn run_async<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    tokio::runtime::Runtime::new()?.block_on(future)
}

#[cfg(test)]
mod tests {
    use super::{
        command_name_for_binary, format_age, format_workspace_info_table, format_workspace_list,
        invoked_binary_name, workspace_info, Cli, Command, ListRow, RuntimeChoice,
    };
    use crate::{
        home::state_dir,
        metadata::{write_metadata, JjMetadata, Metadata},
    };
    use chrono::{Duration, Utc};
    use clap::{CommandFactory, Parser};
    use clap_complete::{generate, Shell};
    use serde_json::Value;
    use std::{ffi::OsStr, fs};

    #[test]
    fn create_accepts_name_and_optional_from_revset() {
        let cli = Cli::try_parse_from(["pando", "create", "demo", "--from", "@-"]).unwrap();

        match cli.command {
            Command::Create { name, from, .. } => {
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
    fn create_accepts_boxlite_runtime_and_image() {
        let cli = Cli::try_parse_from([
            "pando",
            "create",
            "demo",
            "--runtime",
            "boxlite",
            "--image",
            "alpine:3.22",
        ])
        .unwrap();

        match cli.command {
            Command::Create { runtime, image, .. } => {
                assert_eq!(runtime, Some(RuntimeChoice::Boxlite));
                assert_eq!(image.as_deref(), Some("alpine:3.22"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn exec_preserves_argument_boundaries() {
        let cli = Cli::try_parse_from([
            "pando", "exec", "demo", "--", "printf", "%s", "a b", "$(false)",
        ])
        .unwrap();

        match cli.command {
            Command::Exec { name, arguments } => {
                assert_eq!(name, "demo");
                assert_eq!(arguments, ["printf", "%s", "a b", "$(false)"]);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn info_accepts_name_without_json_flag() {
        let cli = Cli::try_parse_from(["pando", "info", "demo"]).unwrap();

        match cli.command {
            Command::Info { name, json } => {
                assert_eq!(name, "demo");
                assert!(!json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn get_alias_parses_as_info() {
        let cli = Cli::try_parse_from(["pd", "get", "demo"]).unwrap();

        match cli.command {
            Command::Info { name, json } => {
                assert_eq!(name, "demo");
                assert!(!json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn info_still_accepts_json_flag_for_compatibility() {
        let cli = Cli::try_parse_from(["pando", "info", "demo", "--json"]).unwrap();

        match cli.command {
            Command::Info { name, json } => {
                assert_eq!(name, "demo");
                assert!(json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn cd_accepts_workspace_name_and_print_flag() {
        let cli = Cli::try_parse_from(["pd", "cd", "demo", "--print"]).unwrap();

        match cli.command {
            Command::Cd { name, print } => {
                assert_eq!(name, "demo");
                assert!(print);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn workspace_info_json_uses_stored_metadata_workspace_path() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let canonical_root = source.path().canonicalize().unwrap();
        fs::create_dir_all(canonical_root.join(".jj/repo")).unwrap();
        let state_dir = state_dir(home.path(), "demo");
        let workspace_path = state_dir.join("workspace");
        let mut metadata = Metadata::new("demo", canonical_root.clone(), workspace_path.clone());
        metadata.jj = Some(JjMetadata {
            workspace_name: Some("pando-demo".to_owned()),
            base_commit: Some("1234567890abcdef".to_owned()),
            base_revision: Some("y".to_owned()),
        });
        write_metadata(&state_dir, &metadata).unwrap();

        let info = workspace_info(home.path(), "demo").unwrap();
        let table = format_workspace_info_table(&info);
        let value: Value = serde_json::to_value(&info).unwrap();

        assert!(table.contains("FIELD"));
        assert!(table.contains("VALUE"));
        assert!(table.contains("pando-demo"));
        assert_eq!(value["name"], "demo");
        assert_eq!(value["state_dir"], state_dir.to_string_lossy().as_ref());
        assert_eq!(
            value["workspace_path"],
            workspace_path.to_string_lossy().as_ref()
        );
        assert_eq!(
            value["canonical_root"],
            canonical_root.to_string_lossy().as_ref()
        );
        assert!(value["created_at"].is_string());
        assert_eq!(value["jj"]["workspace_name"], "pando-demo");
        assert_eq!(value["jj"]["base_commit"], "1234567890abcdef");
        assert_eq!(value["jj"]["base_revision"], "y");
        assert_eq!(
            value["jj"]["repo_path"],
            canonical_root.join(".jj/repo").to_string_lossy().as_ref()
        );
    }

    #[test]
    fn list_helpers_use_product_display_values() {
        let now = Utc::now();
        assert_eq!(format_age(now - Duration::seconds(3), now), "3s");
        assert_eq!(format_age(now - Duration::minutes(2), now), "2m");
        assert_eq!(format_age(now - Duration::hours(4), now), "4h");
        assert_eq!(format_age(now - Duration::days(5), now), "5d");
    }

    #[test]
    fn format_workspace_list_aligns_columns_for_mixed_name_lengths() {
        let table = format_workspace_list(&[
            ListRow {
                name: "long-workspace-name".to_owned(),
                age: "1h".to_owned(),
                base: "y".to_owned(),
                jj: "pando-long-workspace-name".to_owned(),
            },
            ListRow {
                name: "short-name".to_owned(),
                age: "10m".to_owned(),
                base: "krs".to_owned(),
                jj: "pando-short-name".to_owned(),
            },
        ]);

        assert_eq!(
            table,
            "NAME                 AGE  BASE  JJ\n\
             long-workspace-name   1h  y     pando-long-workspace-name\n\
             short-name           10m  krs   pando-short-name\n"
        );
    }

    #[test]
    fn remove_accepts_keep_jj_workspace_flag() {
        let cli = Cli::try_parse_from(["pando", "remove", "demo", "--keep-jj-workspace"]).unwrap();

        match cli.command {
            Command::Remove {
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
    fn rm_alias_parses_as_remove() {
        let cli = Cli::try_parse_from(["pando", "rm", "demo"]).unwrap();

        match cli.command {
            Command::Remove {
                name,
                keep_jj_workspace,
            } => {
                assert_eq!(name, "demo");
                assert!(!keep_jj_workspace);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn completions_accepts_shell_and_generates_bash_script() {
        let cli = Cli::try_parse_from(["pando", "completions", "bash"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Completions { shell: Shell::Bash }
        ));

        let mut command = Cli::command().name("pando");
        let mut output = Vec::new();
        generate(Shell::Bash, &mut command, "pando", &mut output);
        let script = String::from_utf8(output).unwrap();

        assert!(script.contains("_pando"));
        assert!(script.contains("remove"));
        assert!(script.contains("completions"));
    }

    #[test]
    fn help_uses_requested_binary_name_and_avoids_implementation_details() {
        let mut command = Cli::command().name("pd");
        let help = command.render_help().to_string();

        assert!(help.contains("Usage: pd"));
        assert!(!help.contains("Usage: pando"));

        assert!(help.contains("--version"));
        assert!(!help.contains("implementation"));
        assert!(!help.contains("native"));
        assert!(help.contains("remove"));
        assert!(!help.contains("destroy"));
    }

    #[test]
    fn invoked_binary_name_uses_file_name_or_defaults_to_pando() {
        assert_eq!(
            invoked_binary_name(Some(OsStr::new("/usr/local/bin/pd"))),
            "pd"
        );
        assert_eq!(invoked_binary_name(Some(OsStr::new("pando"))), "pando");
        assert_eq!(invoked_binary_name(None), "pando");
    }

    #[test]
    fn command_name_for_binary_supports_pd_alias() {
        assert_eq!(command_name_for_binary("pd"), "pd");
        assert_eq!(command_name_for_binary("pando"), "pando");
        assert_eq!(command_name_for_binary("other"), "pando");
    }
}

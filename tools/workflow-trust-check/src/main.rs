use anyhow::{bail, Context, Result};
use serde_yaml::{Mapping, Value};
use std::{collections::BTreeSet, env, fs, path::Path};

const REQUIRED_GATE: &str = "github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' && github.ref_protected";
const REQUIRED_ENVIRONMENT: &str = "pando-runtime-qualification";
const DOWNLOAD_ACTION: &str = "actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093";
const JOBS: [(&str, [&str; 4]); 2] = [
    (
        "live-linux-kernel-overlay",
        ["self-hosted", "Linux", "X64", "pando-kvm-root"],
    ),
    (
        "live-linux-fuse-overlay",
        ["self-hosted", "Linux", "X64", "pando-kvm-rootless"],
    ),
];

fn field<'a>(mapping: &'a Mapping, key: &str) -> Result<&'a Value> {
    mapping
        .get(Value::String(key.to_owned()))
        .with_context(|| format!("missing {key}"))
}

fn string_field<'a>(mapping: &'a Mapping, key: &str) -> Result<&'a str> {
    field(mapping, key)?
        .as_str()
        .with_context(|| format!("{key} must be a string"))
}

fn check_pinned_action(job: &str, action: &str) -> Result<()> {
    if action.starts_with("./") || action.starts_with("docker://") {
        bail!("job {job} uses disallowed local or Docker action {action:?}");
    }
    let (repository, revision) = action
        .rsplit_once('@')
        .with_context(|| format!("job {job} action {action:?} has no revision"))?;
    let repository_parts = repository.split('/').collect::<Vec<_>>();
    if repository_parts.len() < 2 || repository_parts.iter().any(|part| part.is_empty()) {
        bail!("job {job} action {action:?} is not an owner/repository action");
    }
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("job {job} action {action:?} is not pinned to an exact 40-hex commit SHA");
    }
    Ok(())
}

fn check_job(name: &str, job: &Mapping, expected_labels: &[&str]) -> Result<()> {
    if string_field(job, "if").with_context(|| format!("job {name}"))? != REQUIRED_GATE {
        bail!("job {name} has an unsafe if gate");
    }
    if string_field(job, "environment").with_context(|| format!("job {name}"))?
        != REQUIRED_ENVIRONMENT
    {
        bail!("job {name} has the wrong protected environment");
    }

    let labels = field(job, "runs-on")?
        .as_sequence()
        .with_context(|| format!("job {name} runs-on must be a label sequence"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .with_context(|| format!("job {name} contains a non-string runner label"))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let expected = expected_labels
        .iter()
        .map(|label| (*label).to_owned())
        .collect::<BTreeSet<_>>();
    if labels != expected {
        bail!("job {name} has unexpected self-hosted runner labels: {labels:?}");
    }

    let permissions = field(job, "permissions")?
        .as_mapping()
        .with_context(|| format!("job {name} permissions must be a mapping"))?;
    if permissions.len() != 2
        || string_field(permissions, "actions")? != "read"
        || string_field(permissions, "contents")? != "read"
    {
        bail!("job {name} permissions must be exactly actions:read and contents:read");
    }

    let steps = field(job, "steps")?
        .as_sequence()
        .with_context(|| format!("job {name} steps must be a sequence"))?;
    let actions = steps
        .iter()
        .filter_map(Value::as_mapping)
        .filter_map(|step| step.get(Value::String("uses".to_owned())))
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for action in &actions {
        check_pinned_action(name, action)?;
    }
    let downloads = actions
        .into_iter()
        .filter(|action| action.starts_with("actions/download-artifact@"))
        .collect::<Vec<_>>();
    if downloads != [DOWNLOAD_ACTION] {
        bail!("job {name} must use exactly the reviewed artifact download action");
    }
    Ok(())
}

fn check_workflow(source: &str) -> Result<()> {
    let document: Value = serde_yaml::from_str(source).context("workflow is not valid YAML")?;
    let root = document
        .as_mapping()
        .context("workflow root must be a mapping")?;
    let jobs = field(root, "jobs")?
        .as_mapping()
        .context("jobs must be a mapping")?;
    for (name, labels) in JOBS {
        let job = jobs
            .get(Value::String(name.to_owned()))
            .with_context(|| format!("missing privileged job {name}"))?
            .as_mapping()
            .with_context(|| format!("job {name} must be a mapping"))?;
        check_job(name, job, &labels)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let path = env::args_os()
        .nth(1)
        .unwrap_or_else(|| ".github/workflows/runtime-experimental.yml".into());
    let source = fs::read_to_string(Path::new(&path))
        .with_context(|| format!("could not read {}", Path::new(&path).display()))?;
    check_workflow(&source)
}

#[cfg(test)]
mod tests {
    use super::{check_workflow, DOWNLOAD_ACTION, REQUIRED_GATE};

    fn job(name: &str, labels: &str) -> String {
        format!(
            "  {name}:\n    if: {REQUIRED_GATE}\n    environment: pando-runtime-qualification\n    permissions:\n      actions: read\n      contents: read\n    runs-on: [{labels}]\n    steps:\n      - uses: {DOWNLOAD_ACTION}\n"
        )
    }

    fn valid() -> String {
        format!(
            "jobs:\n{}{}",
            job(
                "live-linux-kernel-overlay",
                "self-hosted, Linux, X64, pando-kvm-root"
            ),
            job(
                "live-linux-fuse-overlay",
                "self-hosted, Linux, X64, pando-kvm-rootless"
            )
        )
    }

    #[test]
    fn accepts_exact_semantic_policy() {
        check_workflow(&valid()).unwrap();
    }

    #[test]
    fn comments_cannot_smuggle_the_expected_gate() {
        let source = valid().replacen(
            &format!("if: {REQUIRED_GATE}"),
            &format!("if: false # if: {REQUIRED_GATE}"),
            1,
        );
        assert!(check_workflow(&source).is_err());
    }

    #[test]
    fn rejects_write_permission() {
        let source = valid().replacen("actions: read", "actions: write", 1);
        assert!(check_workflow(&source).is_err());
    }

    #[test]
    fn rejects_misplaced_or_wrong_download_action() {
        let source = valid().replacen(
            &format!("- uses: {DOWNLOAD_ACTION}"),
            &format!("# - uses: {DOWNLOAD_ACTION}\n      - uses: actions/download-artifact@wrong"),
            1,
        );
        assert!(check_workflow(&source).is_err());
    }

    #[test]
    fn rejects_missing_environment_even_if_comment_mentions_it() {
        let source = valid().replacen(
            "environment: pando-runtime-qualification",
            "# environment: pando-runtime-qualification",
            1,
        );
        assert!(check_workflow(&source).is_err());
    }

    #[test]
    fn rejects_every_unpinned_or_unreviewable_action_form() {
        for action in [
            "attacker/action@main",
            "attacker/action@123456789012345678901234567890123456789",
            "attacker/action@12345678901234567890123456789012345678901",
            "attacker/action@gggggggggggggggggggggggggggggggggggggggg",
            "./attacker-action",
            "docker://attacker/image:latest",
        ] {
            let source = valid().replacen(
                &format!("- uses: {DOWNLOAD_ACTION}"),
                &format!("- uses: {action}\n      - uses: {DOWNLOAD_ACTION}"),
                1,
            );
            assert!(
                check_workflow(&source).is_err(),
                "unsafe action form was accepted: {action}"
            );
        }
    }
}

use pando::runtime::{RuntimePolicy, RuntimeSeccompPolicy};
use std::process::Command;

pub fn qualified_runtime_policy() -> RuntimePolicy {
    RuntimePolicy {
        seccomp: qualified_seccomp_policy(),
        ..RuntimePolicy::default()
    }
}

#[cfg(target_os = "linux")]
fn qualified_seccomp_policy() -> RuntimeSeccompPolicy {
    RuntimeSeccompPolicy::AllowUnqualifiedProvider
}

#[cfg(target_os = "macos")]
fn qualified_seccomp_policy() -> RuntimeSeccompPolicy {
    RuntimeSeccompPolicy::Required
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn qualified_seccomp_policy() -> RuntimeSeccompPolicy {
    panic!("BoxLite live qualification supports only Linux/KVM and macOS/HVF")
}

pub fn add_qualified_runtime_cli_args(command: &mut Command) {
    #[cfg(target_os = "linux")]
    command.arg("--allow-unqualified-seccomp");
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    panic!("BoxLite live qualification supports only Linux/KVM and macOS/HVF");
}

pub fn expected_seccomp_json() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "allow-unqualified-provider"
    }
    #[cfg(target_os = "macos")]
    {
        "required"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    panic!("BoxLite live qualification supports only Linux/KVM and macOS/HVF");
}

#[cfg(test)]
mod tests {
    #[test]
    fn direct_policy_matches_platform_qualification() {
        let policy = super::qualified_runtime_policy();
        #[cfg(target_os = "linux")]
        assert_eq!(
            policy.seccomp,
            pando::runtime::RuntimeSeccompPolicy::AllowUnqualifiedProvider
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            policy.seccomp,
            pando::runtime::RuntimeSeccompPolicy::Required
        );
    }

    #[test]
    fn cli_args_match_platform_qualification() {
        let mut command = std::process::Command::new("pando");
        super::add_qualified_runtime_cli_args(&mut command);
        let args = command.get_args().collect::<Vec<_>>();
        #[cfg(target_os = "linux")]
        assert_eq!(args, ["--allow-unqualified-seccomp"]);
        #[cfg(target_os = "macos")]
        assert!(args.is_empty());
    }

    #[test]
    fn metadata_expectation_matches_platform_qualification() {
        let expected = super::expected_seccomp_json();
        #[cfg(target_os = "linux")]
        assert_eq!(expected, "allow-unqualified-provider");
        #[cfg(target_os = "macos")]
        assert_eq!(expected, "required");
    }
}

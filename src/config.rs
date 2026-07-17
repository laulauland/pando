use anyhow::{Context, Result};
use serde::Deserialize;
use std::{fs, io::ErrorKind, path::Path};

const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub runtime: RuntimeDefaults,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDefaults {
    pub runtime: Option<ConfiguredRuntime>,
    pub image: Option<String>,
    pub cpus: Option<u8>,
    pub memory_mib: Option<u32>,
    pub allow_unqualified_seccomp: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfiguredRuntime {
    Boxlite,
}

pub fn read_config(home: &Path) -> Result<Config> {
    let path = home.join(CONFIG_FILE);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()))
        }
    };

    toml::from_str(&contents).with_context(|| format!("could not parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{read_config, ConfiguredRuntime};

    #[test]
    fn missing_config_uses_empty_defaults() {
        let home = tempfile::tempdir().unwrap();
        let config = read_config(home.path()).unwrap();

        assert_eq!(config.runtime.runtime, None);
        assert_eq!(config.runtime.image, None);
    }

    #[test]
    fn reads_runtime_defaults() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            r#"
[runtime]
runtime = "boxlite"
image = "debian:stable"
cpus = 4
memory_mib = 1024
allow_unqualified_seccomp = true
"#,
        )
        .unwrap();

        let config = read_config(home.path()).unwrap();
        assert_eq!(config.runtime.runtime, Some(ConfiguredRuntime::Boxlite));
        assert_eq!(config.runtime.image.as_deref(), Some("debian:stable"));
        assert_eq!(config.runtime.cpus, Some(4));
        assert_eq!(config.runtime.memory_mib, Some(1024));
        assert_eq!(config.runtime.allow_unqualified_seccomp, Some(true));
    }

    #[test]
    fn rejects_unknown_keys_instead_of_ignoring_typos() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[runtime]\nmemory_mb = 1024\n",
        )
        .unwrap();

        let error = read_config(home.path()).unwrap_err();
        assert!(error.to_string().contains("could not parse"));
    }
}

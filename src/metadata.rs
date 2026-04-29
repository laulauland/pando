use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const METADATA_FILE: &str = "meta.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub canonical_root: PathBuf,
    pub workspace_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jj: Option<JjMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JjMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

impl Metadata {
    pub fn new(name: impl Into<String>, canonical_root: PathBuf, workspace_path: PathBuf) -> Self {
        Self {
            name: name.into(),
            created_at: Utc::now(),
            canonical_root,
            workspace_path,
            jj: None,
        }
    }
}

pub fn metadata_path(state_dir: &Path) -> PathBuf {
    state_dir.join(METADATA_FILE)
}

pub fn write_metadata(state_dir: &Path, metadata: &Metadata) -> Result<()> {
    fs::create_dir_all(state_dir)?;
    fs::write(metadata_path(state_dir), toml::to_string_pretty(metadata)?)?;
    Ok(())
}

pub fn read_metadata(state_dir: &Path) -> Result<Metadata> {
    let contents = fs::read_to_string(metadata_path(state_dir))?;
    Ok(toml::from_str(&contents)?)
}

#[cfg(test)]
mod tests {
    use super::{metadata_path, read_metadata, write_metadata, Metadata};

    #[test]
    fn round_trips_metadata_at_state_dir_root() {
        let state_dir = tempfile::tempdir().unwrap();
        let workspace_path = state_dir.path().join("workspace");
        let metadata = Metadata::new(
            "demo",
            state_dir.path().canonicalize().unwrap(),
            workspace_path.clone(),
        );

        write_metadata(state_dir.path(), &metadata).unwrap();
        assert_eq!(
            metadata_path(state_dir.path()),
            state_dir.path().join("meta.toml")
        );

        let read = read_metadata(state_dir.path()).unwrap();
        assert_eq!(read.name, "demo");
        assert_eq!(read.canonical_root, metadata.canonical_root);
        assert_eq!(read.workspace_path, workspace_path);
        assert!(read.jj.is_none());
    }
}

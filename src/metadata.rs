use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

pub const PANDO_DIR: &str = ".pando";
pub const METADATA_FILE: &str = "metadata.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub canonical_root: PathBuf,
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
    pub fn new(name: impl Into<String>, canonical_root: PathBuf) -> Self {
        Self {
            name: name.into(),
            created_at: Utc::now(),
            canonical_root,
            jj: None,
        }
    }
}

pub fn metadata_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(PANDO_DIR).join(METADATA_FILE)
}

pub fn write_metadata(workspace_path: &Path, metadata: &Metadata) -> Result<()> {
    let path = metadata_path(workspace_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(metadata)?)?;
    Ok(())
}

pub fn read_metadata(workspace_path: &Path) -> Result<Metadata> {
    let bytes = fs::read(metadata_path(workspace_path))?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::{read_metadata, write_metadata, Metadata};

    #[test]
    fn round_trips_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = Metadata::new("demo", dir.path().canonicalize().unwrap());

        write_metadata(dir.path(), &metadata).unwrap();
        let read = read_metadata(dir.path()).unwrap();

        assert_eq!(read.name, "demo");
        assert_eq!(read.canonical_root, metadata.canonical_root);
        assert!(read.jj.is_none());
    }
}

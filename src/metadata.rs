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
    #[serde(
        default,
        alias = "workspace_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
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
    use super::{metadata_path, read_metadata, write_metadata, JjMetadata, Metadata};
    use chrono::{DateTime, Utc};
    use proptest::prelude::*;
    use std::path::{Path, PathBuf};

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

    fn safe_segment() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9_-]{1,12}".prop_map(|s| s)
    }

    fn safe_path_suffix() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(safe_segment(), 1..4)
    }

    fn join_segments(base: &Path, segments: Vec<String>) -> PathBuf {
        segments
            .into_iter()
            .fold(base.to_path_buf(), |path, segment| path.join(segment))
    }

    fn timestamp() -> impl Strategy<Value = DateTime<Utc>> {
        (0i64..4_102_444_800, 0u32..1_000_000_000)
            .prop_map(|(secs, nanos)| DateTime::<Utc>::from_timestamp(secs, nanos).unwrap())
    }

    fn optional_jj_metadata() -> impl Strategy<Value = Option<JjMetadata>> {
        prop::option::of((
            prop::option::of("[a-zA-Z0-9_-]{1,16}"),
            prop::option::of("[a-f0-9]{12,40}"),
        ))
        .prop_map(|jj| {
            jj.map(|(workspace_name, base_commit)| JjMetadata {
                workspace_name,
                base_commit,
            })
        })
    }

    proptest! {
        #[test]
        fn metadata_with_optional_jj_fields_round_trips(
            name in "[a-zA-Z0-9_-]{1,16}",
            created_at in timestamp(),
            jj in optional_jj_metadata(),
            canonical_suffix in safe_path_suffix(),
            workspace_suffix in safe_path_suffix(),
        ) {
            let temp = tempfile::tempdir().unwrap();
            let state_dir = temp.path().join("state");
            let canonical_root = join_segments(temp.path(), canonical_suffix);
            let workspace_path = join_segments(temp.path(), workspace_suffix);
            let metadata = Metadata {
                name,
                created_at,
                canonical_root,
                workspace_path,
                jj,
            };

            write_metadata(&state_dir, &metadata).unwrap();
            let read = read_metadata(&state_dir).unwrap();

            prop_assert_eq!(read, metadata);
        }
    }
}

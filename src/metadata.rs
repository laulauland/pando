use crate::runtime::RuntimeIdentity;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

pub const METADATA_FILE: &str = "meta.toml";
pub const RUNTIME_TRANSACTION_FILE: &str = "runtime-create.toml";
pub const RUNTIME_TRANSACTION_TEMP_FILE: &str = ".runtime-create.toml.tmp";

#[cfg(all(unix, debug_assertions))]
fn injected_journal_crash(point: &str) {
    if std::env::var("PANDO_TEST_CRASH_POINT").as_deref() == Ok(point) {
        // SAFETY: getpid returns this process and SIGKILL is enabled only by explicit debug tests.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    }
}

#[cfg(not(all(unix, debug_assertions)))]
fn injected_journal_crash(_point: &str) {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub canonical_root: PathBuf,
    pub workspace_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jj: Option<JjMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMetadata {
    pub identity: RuntimeIdentity,
    pub image: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeCreateTransaction {
    pub name: String,
    pub provider_name: String,
    pub image: String,
    pub canonical_root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<RuntimeIdentity>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
}

impl Metadata {
    pub fn new(name: impl Into<String>, canonical_root: PathBuf, workspace_path: PathBuf) -> Self {
        Self {
            name: name.into(),
            created_at: Utc::now(),
            canonical_root,
            workspace_path,
            jj: None,
            runtime: None,
        }
    }
}

pub fn metadata_path(state_dir: &Path) -> PathBuf {
    state_dir.join(METADATA_FILE)
}

pub fn write_metadata(state_dir: &Path, metadata: &Metadata) -> Result<()> {
    fs::create_dir_all(state_dir)?;
    let destination = metadata_path(state_dir);
    let mut file = NamedTempFile::new_in(state_dir)?;
    file.write_all(toml::to_string_pretty(metadata)?.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(destination).map_err(|error| error.error)?;
    fs::File::open(state_dir)?.sync_all()?;
    Ok(())
}

pub fn runtime_transaction_path(home: &Path, name: &str) -> PathBuf {
    home.join("transactions")
        .join(name)
        .join(RUNTIME_TRANSACTION_FILE)
}

pub fn has_runtime_transaction(home: &Path, name: &str) -> Result<bool> {
    crate::naming::validate_name(name)?;
    Ok(runtime_transaction_directories(home)?
        .iter()
        .any(|directory| directory.name() == name))
}

pub fn has_any_runtime_transactions(home: &Path) -> Result<bool> {
    Ok(!runtime_transaction_directories(home)?.is_empty())
}

#[cfg(unix)]
pub(crate) struct RuntimeTransactionDirectory {
    root: fs::File,
    directory: fs::File,
    name: String,
    path: PathBuf,
}

#[cfg(not(unix))]
pub(crate) struct RuntimeTransactionDirectory;

#[cfg(not(unix))]
impl RuntimeTransactionDirectory {
    pub(crate) fn name(&self) -> &str {
        ""
    }
    pub(crate) fn read(&self) -> Result<Option<RuntimeCreateTransaction>> {
        anyhow::bail!(
            "safe runtime transaction directory capabilities are unsupported on this platform"
        )
    }
    pub(crate) fn clear(&self) -> Result<()> {
        anyhow::bail!(
            "safe runtime transaction directory capabilities are unsupported on this platform"
        )
    }
}

#[cfg(unix)]
impl RuntimeTransactionDirectory {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    #[cfg(feature = "microvm-boxlite")]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn read(&self) -> Result<Option<RuntimeCreateTransaction>> {
        let Some(file) = openat_file(&self.directory, RUNTIME_TRANSACTION_FILE, libc::O_RDONLY)?
        else {
            return Ok(None);
        };
        validate_private_file_handle(&file, &self.path.join(RUNTIME_TRANSACTION_FILE))?;
        use std::io::Read as _;
        let mut contents = String::new();
        std::io::BufReader::new(file).read_to_string(&mut contents)?;
        Ok(Some(toml::from_str(&contents)?))
    }

    #[cfg(feature = "microvm-boxlite")]
    pub(crate) fn contains_only_optional_temp(&self) -> Result<bool> {
        let entries = fs::read_dir(&self.path)?.collect::<std::io::Result<Vec<_>>>()?;
        revalidate_open_directory(&self.directory, &self.path)?;
        Ok(entries
            .iter()
            .all(|entry| entry.file_name() == RUNTIME_TRANSACTION_TEMP_FILE))
    }

    pub(crate) fn clear(&self) -> Result<()> {
        unlinkat_optional(&self.directory, RUNTIME_TRANSACTION_FILE)?;
        self.directory.sync_all()?;
        injected_journal_crash("journal-cleanup-published-unlinked");
        unlinkat_optional(&self.directory, RUNTIME_TRANSACTION_TEMP_FILE)?;
        self.directory.sync_all()?;
        injected_journal_crash("journal-cleanup-temp-unlinked");
        unlinkat_directory(&self.root, &self.name)?;
        self.root.sync_all()?;
        injected_journal_crash("journal-cleanup-dir-unlinked");
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn runtime_transaction_directories(
    home: &Path,
) -> Result<Vec<RuntimeTransactionDirectory>> {
    let home_fd = open_directory_path(home)?;
    let Some(root) = openat_directory_optional(&home_fd, "transactions")? else {
        return Ok(Vec::new());
    };
    validate_private_file_handle_mode(&root, &home.join("transactions"), 0o700, true)?;
    let root_path = home.join("transactions");
    let entries = fs::read_dir(&root_path)?.collect::<std::io::Result<Vec<_>>>()?;
    revalidate_open_directory(&root, &root_path)?;
    let mut directories = Vec::new();
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("runtime transaction directory is not UTF-8"))?;
        crate::naming::validate_name(&name)?;
        let directory = openat_directory(&root, &name)?;
        let path = root_path.join(&name);
        validate_private_file_handle_mode(&directory, &path, 0o700, true)?;
        directories.push(RuntimeTransactionDirectory {
            root: root.try_clone()?,
            directory,
            name,
            path,
        });
    }
    directories.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(directories)
}

#[cfg(not(unix))]
pub(crate) fn runtime_transaction_directories(
    _home: &Path,
) -> Result<Vec<RuntimeTransactionDirectory>> {
    anyhow::bail!(
        "safe runtime transaction directory capabilities are unsupported on this platform"
    )
}

pub fn write_runtime_transaction(
    home: &Path,
    transaction: &RuntimeCreateTransaction,
) -> Result<()> {
    crate::naming::validate_name(&transaction.name)?;
    #[cfg(not(unix))]
    anyhow::bail!("safe runtime transaction writes are unsupported on this platform");
    #[cfg(unix)]
    let (home_fd, root, directory) = open_or_create_transaction_directory(home, &transaction.name)?;
    #[cfg(unix)]
    let mut file = createat_file(&directory, RUNTIME_TRANSACTION_TEMP_FILE)?;
    injected_journal_crash("journal-temp-created");
    file.write_all(toml::to_string_pretty(transaction)?.as_bytes())?;
    file.sync_all()?;
    injected_journal_crash("journal-temp-written");
    if let Some(existing) = openat_file(&directory, RUNTIME_TRANSACTION_FILE, libc::O_RDONLY)? {
        validate_private_file_handle(
            &existing,
            &runtime_transaction_path(home, &transaction.name),
        )?;
    }
    renameat(
        &directory,
        RUNTIME_TRANSACTION_TEMP_FILE,
        RUNTIME_TRANSACTION_FILE,
    )?;
    directory.sync_all()?;
    root.sync_all()?;
    home_fd.sync_all()?;
    injected_journal_crash("journal-published");
    Ok(())
}

pub fn read_runtime_transaction(path: &Path) -> Result<RuntimeCreateTransaction> {
    let home = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| anyhow::anyhow!("runtime transaction path has no home"))?;
    let name = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("runtime transaction path has no valid name"))?;
    let directory = runtime_transaction_directories(home)?
        .into_iter()
        .find(|directory| directory.name() == name)
        .ok_or_else(|| anyhow::anyhow!("runtime transaction not found"))?;
    directory
        .read()?
        .ok_or_else(|| anyhow::anyhow!("runtime transaction journal is missing"))
}

pub fn clear_runtime_transaction(home: &Path, name: &str) -> Result<()> {
    crate::naming::validate_name(name)?;
    if let Some(directory) = runtime_transaction_directories(home)?
        .into_iter()
        .find(|directory| directory.name() == name)
    {
        directory.clear()?;
    }
    Ok(())
}

#[cfg(unix)]
fn c_name(name: &str) -> Result<std::ffi::CString> {
    Ok(std::ffi::CString::new(name)?)
}

#[cfg(unix)]
fn open_directory_path(path: &Path) -> Result<fs::File> {
    use std::{os::fd::FromRawFd, os::unix::ffi::OsStrExt};
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: path is NUL-terminated; returned descriptor is uniquely owned below.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fd is a newly opened descriptor owned by this function.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn openat_directory_optional(parent: &fs::File, name: &str) -> Result<Option<fs::File>> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    // SAFETY: parent and name are valid for this call; returned descriptor is handled below.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.into());
    }
    // SAFETY: fd is a newly opened descriptor owned by this function.
    Ok(Some(unsafe { fs::File::from_raw_fd(fd) }))
}

#[cfg(unix)]
fn openat_directory(parent: &fs::File, name: &str) -> Result<fs::File> {
    openat_directory_optional(parent, name)?
        .ok_or_else(|| anyhow::anyhow!("runtime transaction directory disappeared"))
}

#[cfg(unix)]
fn mkdirat_private(parent: &fs::File, name: &str) -> Result<()> {
    use std::os::fd::AsRawFd;
    let name = c_name(name)?;
    // SAFETY: parent and name are valid and mode is a plain permission mask.
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn open_or_create_transaction_directory(
    home: &Path,
    name: &str,
) -> Result<(fs::File, fs::File, fs::File)> {
    let home_fd = open_directory_path(home)?;
    mkdirat_private(&home_fd, "transactions")?;
    let root = openat_directory(&home_fd, "transactions")?;
    validate_private_file_handle_mode(&root, &home.join("transactions"), 0o700, true)?;
    mkdirat_private(&root, name)?;
    let directory = openat_directory(&root, name)?;
    validate_private_file_handle_mode(
        &directory,
        &home.join("transactions").join(name),
        0o700,
        true,
    )?;
    Ok((home_fd, root, directory))
}

#[cfg(unix)]
fn createat_file(parent: &fs::File, name: &str) -> Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    // SAFETY: parent and name are valid; O_EXCL and O_NOFOLLOW prevent replacement/following.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fd is a newly opened descriptor owned by this function.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn openat_file(parent: &fs::File, name: &str, flags: i32) -> Result<Option<fs::File>> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    // SAFETY: parent and name are valid; returned descriptor is handled below.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.into());
    }
    // SAFETY: fd is a newly opened descriptor owned by this function.
    Ok(Some(unsafe { fs::File::from_raw_fd(fd) }))
}

#[cfg(unix)]
fn renameat(parent: &fs::File, from: &str, to: &str) -> Result<()> {
    use std::os::fd::AsRawFd;
    let from = c_name(from)?;
    let to = c_name(to)?;
    // SAFETY: both names are relative to the same valid directory descriptor.
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(unix)]
fn unlinkat_optional(parent: &fs::File, name: &str) -> Result<()> {
    use std::os::fd::AsRawFd;
    if let Some(file) = openat_file(parent, name, libc::O_RDONLY)? {
        validate_private_file_handle(&file, Path::new(name))?;
        let name = c_name(name)?;
        // SAFETY: name is relative to the validated directory descriptor.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn unlinkat_directory(parent: &fs::File, name: &str) -> Result<()> {
    use std::os::fd::AsRawFd;
    let name = c_name(name)?;
    // SAFETY: name is relative to the validated root descriptor.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_file_handle_mode(
    file: &fs::File,
    path: &Path,
    mode: u32,
    directory: bool,
) -> Result<()> {
    let metadata = file.metadata()?;
    if directory != metadata.is_dir() {
        anyhow::bail!(
            "runtime transaction path has wrong type: {}",
            path.display()
        );
    }
    validate_private_metadata(&metadata, path, mode)
}

#[cfg(unix)]
fn revalidate_open_directory(file: &fs::File, path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata()?;
    let current = open_directory_path(path)?.metadata()?;
    if opened.dev() != current.dev() || opened.ino() != current.ino() {
        anyhow::bail!(
            "runtime transaction directory changed during capability handoff: {}",
            path.display()
        );
    }
    Ok(())
}

fn validate_private_file_handle(file: &fs::File, path: &Path) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        anyhow::bail!(
            "runtime transaction path is not a regular file: {}",
            path.display()
        );
    }
    validate_private_metadata(&metadata, path, 0o600)
}

#[cfg(unix)]
fn validate_private_metadata(
    metadata: &fs::Metadata,
    path: &Path,
    expected_mode: u32,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: geteuid has no preconditions and only reads process credentials.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid || metadata.mode() & 0o777 != expected_mode {
        anyhow::bail!(
            "runtime transaction path has unsafe ownership or permissions: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_metadata(
    _metadata: &fs::Metadata,
    _path: &Path,
    _expected_mode: u32,
) -> Result<()> {
    Ok(())
}

pub fn read_metadata(state_dir: &Path) -> Result<Metadata> {
    let contents = fs::read_to_string(metadata_path(state_dir))?;
    Ok(toml::from_str(&contents)?)
}

#[cfg(test)]
mod tests {
    use super::{
        clear_runtime_transaction, metadata_path, read_metadata, read_runtime_transaction,
        runtime_transaction_path, write_metadata, write_runtime_transaction, JjMetadata, Metadata,
        RuntimeCreateTransaction, RuntimeMetadata,
    };
    use crate::runtime::RuntimeIdentity;
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
        assert!(read.runtime.is_none());
    }

    #[test]
    fn reads_metadata_written_before_runtime_support() {
        let state_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            metadata_path(state_dir.path()),
            r#"name = "demo"
created_at = "2025-01-02T03:04:05Z"
canonical_root = "/source"
workspace_path = "/workspace"
"#,
        )
        .unwrap();

        let read = read_metadata(state_dir.path()).unwrap();
        assert!(read.runtime.is_none());
    }

    #[test]
    fn round_trips_runtime_identity_and_image() {
        let state_dir = tempfile::tempdir().unwrap();
        let mut metadata = Metadata::new(
            "demo",
            state_dir.path().join("source"),
            state_dir.path().join("workspace"),
        );
        metadata.runtime = Some(RuntimeMetadata {
            identity: RuntimeIdentity::new("box-123"),
            image: "alpine:3.22".to_owned(),
        });

        write_metadata(state_dir.path(), &metadata).unwrap();
        assert_eq!(read_metadata(state_dir.path()).unwrap(), metadata);
    }

    #[test]
    fn atomically_round_trips_and_clears_runtime_create_transaction() {
        let home = tempfile::tempdir().unwrap();
        let transaction = RuntimeCreateTransaction {
            name: "demo".to_owned(),
            provider_name: "pando-demo-random-token".to_owned(),
            image: "alpine:3.22".to_owned(),
            canonical_root: home.path().join("source"),
            identity: Some(RuntimeIdentity::new("box-123")),
        };

        write_runtime_transaction(home.path(), &transaction).unwrap();
        let path = runtime_transaction_path(home.path(), "demo");
        assert_eq!(read_runtime_transaction(&path).unwrap(), transaction);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );

        clear_runtime_transaction(home.path(), "demo").unwrap();
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[cfg(unix)]
    #[test]
    fn transaction_write_rejects_symlinked_directories_and_temp_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let home = tempfile::tempdir().unwrap();
        let victim = tempfile::tempdir().unwrap();
        let transaction = RuntimeCreateTransaction {
            name: "demo".to_owned(),
            provider_name: "owned-token".to_owned(),
            image: "alpine:3.22".to_owned(),
            canonical_root: home.path().join("source"),
            identity: None,
        };

        symlink(victim.path(), home.path().join("transactions")).unwrap();
        assert!(write_runtime_transaction(home.path(), &transaction).is_err());
        assert_eq!(std::fs::read_dir(victim.path()).unwrap().count(), 0);
        std::fs::remove_file(home.path().join("transactions")).unwrap();

        std::fs::create_dir(home.path().join("transactions")).unwrap();
        std::fs::set_permissions(
            home.path().join("transactions"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        symlink(victim.path(), home.path().join("transactions/demo")).unwrap();
        assert!(write_runtime_transaction(home.path(), &transaction).is_err());
        std::fs::remove_file(home.path().join("transactions/demo")).unwrap();

        std::fs::create_dir(home.path().join("transactions/demo")).unwrap();
        std::fs::set_permissions(
            home.path().join("transactions/demo"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let victim_file = victim.path().join("file");
        std::fs::write(&victim_file, "untouched").unwrap();
        symlink(
            &victim_file,
            home.path()
                .join("transactions/demo")
                .join(super::RUNTIME_TRANSACTION_TEMP_FILE),
        )
        .unwrap();
        assert!(write_runtime_transaction(home.path(), &transaction).is_err());
        assert_eq!(std::fs::read_to_string(victim_file).unwrap(), "untouched");
    }

    #[cfg(unix)]
    #[test]
    fn transaction_read_rejects_symlink_and_permissive_objects() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join("transactions/demo");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let journal = directory.join(super::RUNTIME_TRANSACTION_FILE);
        let victim = home.path().join("victim");
        std::fs::write(&victim, "name = 'victim'").unwrap();
        symlink(&victim, &journal).unwrap();
        assert!(read_runtime_transaction(&journal).is_err());
        std::fs::remove_file(&journal).unwrap();

        std::fs::write(&journal, "name = 'demo'").unwrap();
        std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_runtime_transaction(&journal).is_err());
        std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read_runtime_transaction(&journal).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn transaction_scan_rejects_unsafe_roots_and_every_unknown_child_type() {
        use std::{
            ffi::CString,
            os::unix::fs::{symlink, PermissionsExt},
        };

        for case in ["dangling-root", "live-root", "file", "symlink", "fifo"] {
            let home = tempfile::tempdir().unwrap();
            let root = home.path().join("transactions");
            let victim = tempfile::tempdir().unwrap();
            match case {
                "dangling-root" => symlink(home.path().join("missing"), &root).unwrap(),
                "live-root" => symlink(victim.path(), &root).unwrap(),
                child => {
                    std::fs::create_dir(&root).unwrap();
                    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                    let entry = root.join("unsafe");
                    match child {
                        "file" => std::fs::write(entry, "unknown").unwrap(),
                        "symlink" => symlink(victim.path(), entry).unwrap(),
                        "fifo" => {
                            let entry = CString::new(entry.as_os_str().as_encoded_bytes()).unwrap();
                            // SAFETY: entry is a valid NUL-terminated path and mkfifo does not retain it.
                            assert_eq!(unsafe { libc::mkfifo(entry.as_ptr(), 0o600) }, 0);
                        }
                        _ => unreachable!(),
                    }
                }
            }
            assert!(
                super::runtime_transaction_directories(home.path()).is_err(),
                "{case}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn opened_transaction_capability_stays_confined_after_root_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let transaction = RuntimeCreateTransaction {
            name: "demo".to_owned(),
            provider_name: "owned-token".to_owned(),
            image: "alpine:3.22".to_owned(),
            canonical_root: home.path().join("source"),
            identity: None,
        };
        write_runtime_transaction(home.path(), &transaction).unwrap();
        let capability = super::runtime_transaction_directories(home.path())
            .unwrap()
            .pop()
            .unwrap();

        std::fs::rename(
            home.path().join("transactions"),
            home.path().join("old-transactions"),
        )
        .unwrap();
        let replacement = home.path().join("transactions/demo");
        std::fs::create_dir_all(&replacement).unwrap();
        std::fs::set_permissions(
            home.path().join("transactions"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o700)).unwrap();
        let sentinel = replacement.join("sentinel");
        std::fs::write(&sentinel, "replacement untouched").unwrap();

        assert_eq!(capability.read().unwrap(), Some(transaction));
        capability.clear().unwrap();
        assert!(!home.path().join("old-transactions/demo").exists());
        assert_eq!(
            std::fs::read_to_string(sentinel).unwrap(),
            "replacement untouched"
        );
    }

    fn safe_path_suffix() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec("[a-zA-Z0-9_-]{1,12}", 1..4)
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
        prop::option::of(
            (
                prop::option::of("[a-zA-Z0-9_-]{1,16}"),
                prop::option::of("[a-f0-9]{12,40}"),
                prop::option::of("[0-9a-z]{1,12}"),
            )
                .prop_map(|(workspace_name, base_commit, base_revision)| JjMetadata {
                    workspace_name,
                    base_commit,
                    base_revision,
                }),
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

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
                runtime: None,
            };

            write_metadata(&state_dir, &metadata).unwrap();
            let read = read_metadata(&state_dir).unwrap();

            prop_assert_eq!(read, metadata);
        }
    }
}

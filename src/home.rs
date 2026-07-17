use anyhow::{anyhow, bail, Result};
use fs2::FileExt;
use std::{
    env,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

pub const PANDO_HOME_ENV: &str = "PANDO_HOME";
const DEFAULT_RELATIVE_HOME: &str = ".pando";
const LEGACY_DEFAULT_RELATIVE_HOME: &str = ".local/state/pando";
const STATE_DIR: &str = "state";
const WORKSPACES_DIR: &str = "workspaces";
const LOCK_FILE: &str = ".lock";

pub fn pando_home() -> Result<PathBuf> {
    if let Some(value) = env::var_os(PANDO_HOME_ENV) {
        if value.is_empty() {
            bail!("{PANDO_HOME_ENV} must not be empty");
        }
        return Ok(PathBuf::from(value));
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(DEFAULT_RELATIVE_HOME))
}

pub fn legacy_pando_home() -> Result<PathBuf> {
    if let Some(value) = env::var_os(PANDO_HOME_ENV) {
        if value.is_empty() {
            bail!("{PANDO_HOME_ENV} must not be empty");
        }
        return Ok(PathBuf::from(value));
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(LEGACY_DEFAULT_RELATIVE_HOME))
}

pub fn ensure_home(home: &Path) -> Result<()> {
    fs::create_dir_all(state_root(home))?;
    fs::create_dir_all(workspaces_root(home))?;
    Ok(())
}

pub fn state_root(home: &Path) -> PathBuf {
    home.join(STATE_DIR)
}

pub fn workspaces_root(home: &Path) -> PathBuf {
    home.join(WORKSPACES_DIR)
}

pub fn state_dir(home: &Path, name: &str) -> PathBuf {
    state_root(home).join(name)
}

pub fn workspace_dir(home: &Path, name: &str) -> PathBuf {
    workspaces_root(home).join(name)
}

pub struct PandoLock {
    file: File,
}

impl PandoLock {
    pub fn acquire(home: &Path) -> Result<Self> {
        Self::acquire_in(&state_root(home))
    }

    pub fn acquire_legacy(home: &Path) -> Result<Self> {
        Self::acquire_in(home)
    }

    fn acquire_in(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(LOCK_FILE))?;
        file.lock_exclusive()?;
        Ok(Self { file })
    }
}

impl Drop for PandoLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

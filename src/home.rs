use anyhow::{anyhow, bail, Result};
use fs2::FileExt;
use std::{
    env,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

pub const PANDO_HOME_ENV: &str = "PANDO_HOME";
const DEFAULT_RELATIVE_HOME: &str = ".local/state/pando";
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

pub fn ensure_home(home: &Path) -> Result<()> {
    fs::create_dir_all(home)?;
    Ok(())
}

pub fn state_dir(home: &Path, name: &str) -> PathBuf {
    home.join(name)
}

pub struct PandoLock {
    file: File,
}

impl PandoLock {
    pub fn acquire(home: &Path) -> Result<Self> {
        fs::create_dir_all(home)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(home.join(LOCK_FILE))?;
        file.lock_exclusive()?;
        Ok(Self { file })
    }
}

impl Drop for PandoLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

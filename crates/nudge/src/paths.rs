use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;

/// Application id, also used as D-Bus name and desktop entry.
pub const APP_ID: &str = "io.github.alfredch.Nudge";

/// XDG locations used by nudge.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub state: PathBuf,
}

impl Paths {
    pub fn new() -> Result<Self> {
        let dirs = ProjectDirs::from("io.github", "alfredch", "nudge")
            .context("cannot determine the home directory")?;
        let state = dirs
            .state_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dirs.data_local_dir().join("state"));
        Ok(Self {
            data: dirs.data_dir().to_path_buf(),
            state,
        })
    }

    pub fn history_db(&self) -> PathBuf {
        self.data.join("history.db")
    }

    pub fn credentials_file(&self) -> PathBuf {
        self.state.join("credentials.json")
    }
}

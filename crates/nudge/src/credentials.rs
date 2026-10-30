//! Storage of the session secret and device id.
//!
//! Primary backend is the Secret Service (GNOME Keyring, KWallet, ...).
//! Without one, a `0600` file in the XDG state directory is used.
//! The account password is never stored.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};
use nudge_api::{DeviceId, Secret};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::paths::{APP_ID, Paths};

const ATTRIBUTES: [(&str, &str); 2] = [("application", APP_ID), ("type", "session")];
const LABEL: &str = "Nudge session";

#[derive(Debug, Clone)]
pub struct Credentials {
    pub device_id: DeviceId,
    pub secret: Secret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Keyring,
    File,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    device_id: String,
    secret: String,
}

impl Credentials {
    fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(&Stored {
            device_id: self.device_id.as_str().to_owned(),
            secret: self.secret.expose().to_owned(),
        })?)
    }

    fn from_json(bytes: &[u8]) -> Result<Self> {
        let stored: Stored =
            serde_json::from_slice(bytes).context("stored credentials are corrupt")?;
        Ok(Self {
            device_id: DeviceId::new(stored.device_id),
            secret: Secret::new(stored.secret),
        })
    }
}

async fn keyring() -> Result<oo7::Keyring> {
    let keyring = oo7::Keyring::new().await?;
    keyring.unlock().await?;
    Ok(keyring)
}

pub async fn load(paths: &Paths) -> Result<Option<(Credentials, Backend)>> {
    match keyring().await {
        Ok(keyring) => {
            let items = keyring.search_items(&ATTRIBUTES).await?;
            if let Some(item) = items.first() {
                let secret = item.secret().await?;
                return Ok(Some((Credentials::from_json(&secret)?, Backend::Keyring)));
            }
        }
        Err(e) => debug!("Secret Service unavailable: {e:#}"),
    }

    let file = paths.credentials_file();
    match fs::read(&file) {
        Ok(bytes) => Ok(Some((Credentials::from_json(&bytes)?, Backend::File))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
    }
}

pub async fn save(paths: &Paths, credentials: &Credentials) -> Result<Backend> {
    let json = credentials.to_json()?;
    let file = paths.credentials_file();
    match keyring().await {
        Ok(keyring) => {
            keyring
                .create_item(LABEL, &ATTRIBUTES, json.as_str(), true)
                .await
                .context("cannot store credentials in the Secret Service")?;
            remove_file(&file)?;
            Ok(Backend::Keyring)
        }
        Err(e) => {
            warn!(
                "Secret Service unavailable ({e:#}), storing credentials in {}",
                file.display()
            );
            write_private(&file, json.as_bytes())?;
            Ok(Backend::File)
        }
    }
}

/// Removes stored credentials from all backends. Returns whether any existed.
pub async fn delete(paths: &Paths) -> Result<bool> {
    let mut found = false;
    match keyring().await {
        Ok(keyring) => {
            found = !keyring.search_items(&ATTRIBUTES).await?.is_empty();
            keyring.delete(&ATTRIBUTES).await?;
        }
        Err(e) => debug!("Secret Service unavailable: {e:#}"),
    }
    found |= remove_file(&paths.credentials_file())?;
    Ok(found)
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    // The mode only applies on creation; fix up an existing file.
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

fn remove_file(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("cannot remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip() {
        let creds = Credentials {
            device_id: DeviceId::new("dev42"),
            secret: Secret::new("s3cr3t"),
        };
        let back = Credentials::from_json(creds.to_json().unwrap().as_bytes()).unwrap();
        assert_eq!(back.device_id, creds.device_id);
        assert_eq!(back.secret, creds.secret);
    }

    #[test]
    fn private_file_has_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/credentials.json");
        write_private(&path, b"{}").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        assert!(remove_file(&path).unwrap());
        assert!(!remove_file(&path).unwrap());
    }
}

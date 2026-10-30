//! Icons and sounds from the Pushover server, cached under `~/.cache/nudge/`.
//!
//! The API requires downloading each file only once. Names are immutable:
//! an app that changes its icon gets a new icon name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nudge_api::{Client, ResourceName};
use tokio::time::timeout;
use tracing::{debug, warn};

/// Messages wait for a missing file; don't let them wait long.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5);
/// A failed name is not requested again within this time.
const RETRY_AFTER: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Icon,
    Sound,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Icon => "icons",
            Kind::Sound => "sounds",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Kind::Icon => "png",
            Kind::Sound => "mp3",
        }
    }
}

pub struct Cache {
    dir: PathBuf,
    client: Client,
    failed: HashMap<(Kind, String), Instant>,
}

impl Cache {
    pub fn new(dir: PathBuf, client: Client) -> Self {
        Self {
            dir,
            client,
            failed: HashMap::new(),
        }
    }

    /// Path of the cached file, downloading it first if needed. `None` if
    /// the name is invalid or the download fails; the message is then shown
    /// without icon or sound.
    pub async fn get(&mut self, kind: Kind, name: &str) -> Option<PathBuf> {
        let Ok(resource) = ResourceName::new(name) else {
            debug!("ignoring invalid {} name {name:?}", kind.dir());
            return None;
        };
        let path = self.path(kind, &resource);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Some(path);
        }
        let key = (kind, name.to_owned());
        if self
            .failed
            .get(&key)
            .is_some_and(|t| t.elapsed() < RETRY_AFTER)
        {
            return None;
        }
        match self.download(kind, &resource, &path).await {
            Ok(()) => {
                debug!("cached {}", path.display());
                self.failed.remove(&key);
                Some(path)
            }
            Err(e) => {
                warn!("cannot download {} {name:?}: {e:#}", kind.dir());
                self.failed.insert(key, Instant::now());
                None
            }
        }
    }

    fn path(&self, kind: Kind, name: &ResourceName) -> PathBuf {
        self.dir
            .join(kind.dir())
            .join(format!("{}.{}", name.as_str(), kind.extension()))
    }

    async fn download(&self, kind: Kind, name: &ResourceName, path: &Path) -> Result<()> {
        let request = async {
            match kind {
                Kind::Icon => self.client.download_icon(name).await,
                Kind::Sound => self.client.download_sound(name).await,
            }
        };
        let data = timeout(DOWNLOAD_TIMEOUT, request)
            .await
            .context("timed out")??;
        write_atomic(path, &data).await
    }
}

/// Writes via a temporary file, so an interrupted download never leaves a
/// truncated file that would be used from then on.
async fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().context("cache path has no parent")?;
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("cannot create {}", dir.display()))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.part", std::process::id()));
    let tmp = PathBuf::from(tmp);
    tokio::fs::write(&tmp, data)
        .await
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e).with_context(|| format!("cannot write {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn setup() -> (MockServer, tempfile::TempDir, Cache) {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let client = Client::with_base_url("nudge-test", &server.uri()).unwrap();
        let cache = Cache::new(dir.path().to_path_buf(), client);
        (server, dir, cache)
    }

    #[tokio::test]
    async fn downloads_once() {
        let (server, dir, mut cache) = setup().await;
        Mock::given(method("GET"))
            .and(path("/icons/abc.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PNG".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let first = cache.get(Kind::Icon, "abc").await.unwrap();
        assert_eq!(first, dir.path().join("icons/abc.png"));
        assert_eq!(std::fs::read(&first).unwrap(), b"PNG");
        assert_eq!(cache.get(Kind::Icon, "abc").await, Some(first));
    }

    #[tokio::test]
    async fn failure_is_remembered() {
        let (server, dir, mut cache) = setup().await;
        Mock::given(method("GET"))
            .and(path("/sounds/nope.mp3"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(cache.get(Kind::Sound, "nope").await, None);
        assert_eq!(cache.get(Kind::Sound, "nope").await, None);
        assert!(!dir.path().join("sounds/nope.mp3").exists());
    }

    #[tokio::test]
    async fn invalid_names_are_not_requested() {
        let (_server, _dir, mut cache) = setup().await;
        assert_eq!(cache.get(Kind::Icon, "../etc/passwd").await, None);
        assert_eq!(cache.get(Kind::Icon, "").await, None);
    }
}

use std::time::Duration;

use reqwest::{Response, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::{Error, Result};
use crate::types::{DeviceId, DeviceName, Fetched, MessageId, Receipt, ResourceName, Secret};

pub const DEFAULT_API_BASE: &str = "https://api.pushover.net/";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Builds the User-Agent the API requires: app name, version and OS.
pub fn user_agent(app: &str, version: &str) -> String {
    format!("{app}/{version} (Linux; unofficial Pushover Open Client)")
}

/// Result of a successful login.
#[derive(Debug, Clone, Deserialize)]
pub struct Login {
    /// The user key. Not needed by the client, but returned by the API.
    #[serde(rename = "id")]
    pub user_key: String,
    pub secret: Secret,
}

/// HTTP client for the Open Client API.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
}

impl Client {
    pub fn new(user_agent: &str) -> Result<Self> {
        Self::with_base_url(user_agent, DEFAULT_API_BASE)
    }

    /// Like [`Client::new`], but against another server (used by tests).
    pub fn with_base_url(user_agent: &str, base: &str) -> Result<Self> {
        let mut base = Url::parse(base)?;
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self { http, base })
    }

    /// Logs in with email and password. Returns [`Error::TwoFactorRequired`]
    /// if the account uses 2FA and `twofa` was not given.
    pub async fn login(&self, email: &str, password: &str, twofa: Option<&str>) -> Result<Login> {
        let mut form = vec![("email", email), ("password", password)];
        if let Some(code) = twofa {
            form.push(("twofa", code));
        }
        let resp = self
            .http
            .post(self.url("1/users/login.json")?)
            .form(&form)
            .send()
            .await?;
        parse(resp).await
    }

    /// Registers this client as a new Open Client device.
    pub async fn register_device(&self, secret: &Secret, name: &DeviceName) -> Result<DeviceId> {
        #[derive(Deserialize)]
        struct Resp {
            id: DeviceId,
        }
        let resp = self
            .http
            .post(self.url("1/devices.json")?)
            .form(&[
                ("secret", secret.expose()),
                ("name", name.as_str()),
                ("os", "O"),
            ])
            .send()
            .await?;
        Ok(parse::<Resp>(resp).await?.id)
    }

    /// Downloads all messages that have not been deleted yet.
    ///
    /// Each entry is decoded on its own; see [`Fetched`].
    pub async fn fetch_messages(&self, secret: &Secret, device: &DeviceId) -> Result<Fetched> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            messages: Vec<serde_json::Value>,
        }
        let resp = self
            .http
            .get(self.url("1/messages.json")?)
            .query(&[("secret", secret.expose()), ("device_id", device.as_str())])
            .send()
            .await?;
        Ok(Fetched::from_raw(parse::<Resp>(resp).await?.messages))
    }

    /// Permanently deletes all messages of the device up to and including `highest`.
    pub async fn delete_up_to(
        &self,
        secret: &Secret,
        device: &DeviceId,
        highest: MessageId,
    ) -> Result<()> {
        let path = format!(
            "1/devices/{}/update_highest_message.json",
            path_segment(device.as_str())
        );
        let resp = self
            .http
            .post(self.url(&path)?)
            .form(&[
                ("secret", secret.expose()),
                ("message", &highest.to_string()),
            ])
            .send()
            .await?;
        parse::<serde_json::Value>(resp).await.map(drop)
    }

    /// Acknowledges an emergency message. Call only on an explicit user action.
    pub async fn acknowledge(&self, secret: &Secret, receipt: &Receipt) -> Result<()> {
        let path = format!(
            "1/receipts/{}/acknowledge.json",
            path_segment(receipt.as_str())
        );
        let resp = self
            .http
            .post(self.url(&path)?)
            .form(&[("secret", secret.expose())])
            .send()
            .await?;
        parse::<serde_json::Value>(resp).await.map(drop)
    }

    /// Downloads an application icon (PNG). Callers must cache the result.
    pub async fn download_icon(&self, icon: &ResourceName) -> Result<Vec<u8>> {
        self.download(&format!("icons/{}.png", icon.as_str())).await
    }

    /// Downloads a notification sound (MP3). Callers must cache the result.
    pub async fn download_sound(&self, sound: &ResourceName) -> Result<Vec<u8>> {
        self.download(&format!("sounds/{}.mp3", sound.as_str()))
            .await
    }

    async fn download(&self, path: &str) -> Result<Vec<u8>> {
        let resp = self.http.get(self.url(path)?).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Api {
                http_status: status,
                errors: Vec::new(),
                request: None,
            });
        }
        Ok(resp.bytes().await?.to_vec())
    }

    fn url(&self, path: &str) -> Result<Url> {
        Ok(self.base.join(path)?)
    }
}

/// Percent-encodes an id for use as a single path segment.
fn path_segment(raw: &str) -> String {
    url::form_urlencoded::byte_serialize(raw.as_bytes()).collect()
}

#[derive(Deserialize)]
struct Envelope {
    status: i64,
    #[serde(default)]
    errors: Option<serde_json::Value>,
    #[serde(default)]
    request: Option<String>,
}

async fn parse<T: DeserializeOwned>(resp: Response) -> Result<T> {
    let http_status = resp.status();
    if http_status == StatusCode::PRECONDITION_FAILED {
        return Err(Error::TwoFactorRequired);
    }
    let body = resp.bytes().await?;
    match serde_json::from_slice::<Envelope>(&body) {
        Ok(env) if env.status == 1 && http_status.is_success() => {
            serde_json::from_slice(&body).map_err(Error::Decode)
        }
        Ok(env) => Err(Error::Api {
            http_status,
            errors: flatten_errors(env.errors),
            request: env.request,
        }),
        Err(_) if !http_status.is_success() => Err(Error::Api {
            http_status,
            errors: Vec::new(),
            request: None,
        }),
        Err(e) => Err(Error::Decode(e)),
    }
}

/// `errors` is documented as an object (`{"name": ["is taken"]}`), other
/// Pushover endpoints use a plain array. Accept both.
fn flatten_errors(errors: Option<serde_json::Value>) -> Vec<String> {
    use serde_json::Value;

    fn text(v: &Value) -> Vec<String> {
        match v {
            Value::String(s) => vec![s.clone()],
            Value::Array(items) => items.iter().flat_map(text).collect(),
            Value::Null => Vec::new(),
            other => vec![other.to_string()],
        }
    }

    match errors {
        Some(Value::Object(map)) => map
            .iter()
            .flat_map(|(key, v)| text(v).into_iter().map(move |msg| format!("{key} {msg}")))
            .collect(),
        Some(other) => text(&other),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn errors_object_and_array() {
        assert_eq!(
            flatten_errors(Some(json!({"name": ["has already been taken"]}))),
            vec!["name has already been taken"]
        );
        assert_eq!(
            flatten_errors(Some(json!(["secret is invalid"]))),
            vec!["secret is invalid"]
        );
        assert!(flatten_errors(None).is_empty());
    }

    #[test]
    fn user_agent_names_app_version_and_os() {
        assert_eq!(
            user_agent("nudge", "0.1.0"),
            "nudge/0.1.0 (Linux; unofficial Pushover Open Client)"
        );
    }
}

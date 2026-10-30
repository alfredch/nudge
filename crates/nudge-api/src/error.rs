use reqwest::StatusCode;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors returned by the API client.
///
/// No variant carries the session secret or a URL containing it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Login needs a two-factor code (HTTP 412). Retry with `twofa` set.
    #[error("two-factor authentication code required")]
    TwoFactorRequired,

    /// The API answered with `status != 1`.
    #[error("API error (HTTP {http_status}): {}", errors.join("; "))]
    Api {
        http_status: StatusCode,
        errors: Vec<String>,
        request: Option<String>,
    },

    /// Transport-level HTTP failure (connect, timeout, TLS, ...).
    #[error("HTTP request failed: {0}")]
    Http(#[source] reqwest::Error),

    /// The response body could not be decoded.
    #[error("unexpected response: {0}")]
    Decode(#[source] serde_json::Error),

    #[error("invalid device name {0:?}: 1-25 characters of [A-Za-z0-9_-] required")]
    InvalidDeviceName(String),

    #[error("invalid resource name {0:?}: only [A-Za-z0-9_-] allowed")]
    InvalidResourceName(String),

    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),

    #[error("WebSocket error: {0}")]
    WebSocket(#[source] Box<tokio_tungstenite::tungstenite::Error>),
}

impl Error {
    /// Whether the request failed with an HTTP 4xx status from the API.
    /// For an established session this usually means the secret or device
    /// is no longer valid and the user has to log in again.
    pub fn is_client_error(&self) -> bool {
        matches!(self, Error::Api { http_status, .. } if http_status.is_client_error())
    }
}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        // GET requests carry the secret in the query string.
        Error::Http(err.without_url())
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        Error::WebSocket(Box::new(err))
    }
}

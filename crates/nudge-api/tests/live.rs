//! Smoke tests against the real Pushover servers with made-up credentials.
//! They register no device. Run explicitly with `cargo test -- --ignored`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use nudge_api::{Client, DEFAULT_PUSH_URL, DeviceId, Error, Frame, PushStream, Secret, user_agent};

fn ua() -> String {
    user_agent("nudge", env!("CARGO_PKG_VERSION"))
}

fn bogus() -> (DeviceId, Secret) {
    (
        DeviceId::new("nudgeSmokeTestInvalidDevice"),
        Secret::new("invalidsecretfornudgesmoketest"),
    )
}

#[tokio::test]
#[ignore = "talks to the real Pushover servers"]
async fn live_websocket_rejects_bogus_login() {
    let (device, secret) = bogus();
    let mut stream = PushStream::connect(DEFAULT_PUSH_URL, &ua(), &device, &secret)
        .await
        .expect("TLS/WebSocket handshake failed");

    let mut frames = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(15), stream.next_frame()).await {
            Ok(Ok(Some(frame))) => frames.push(frame),
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                eprintln!("stream ended with error: {e}");
                break;
            }
            Err(_) => panic!("no reaction within 15 s, frames so far: {frames:?}"),
        }
    }
    eprintln!("frames: {frames:?}");
    assert!(
        frames.contains(&Frame::Error),
        "expected E frame, got {frames:?}"
    );
}

#[tokio::test]
#[ignore = "talks to the real Pushover servers"]
async fn live_fetch_with_bogus_secret() {
    let (device, secret) = bogus();
    let client = Client::new(&ua()).unwrap();
    let err = client.fetch_messages(&secret, &device).await.unwrap_err();
    eprintln!("error: {err:?}");
    assert!(matches!(err, Error::Api { .. }), "{err:?}");
    assert!(err.is_client_error(), "{err:?}");
}

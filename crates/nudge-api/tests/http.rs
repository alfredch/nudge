#![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

use nudge_api::{Client, DeviceId, DeviceName, Error, MessageId, Receipt, ResourceName, Secret};
use serde_json::json;
use wiremock::matchers::{body_string, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const UA: &str = "nudge-test/0.0.0 (Linux; unofficial Pushover Open Client)";
const SECRET: &str = "s3cr3t-value";

async fn setup() -> (MockServer, Client) {
    let server = MockServer::start().await;
    let client = Client::with_base_url(UA, &server.uri()).unwrap();
    (server, client)
}

/// Every mock requires the User-Agent; a request without it gets a 404.
fn mock(m: &str, p: &str) -> wiremock::MockBuilder {
    Mock::given(method(m))
        .and(path(p))
        .and(header("user-agent", UA))
}

fn secret() -> Secret {
    Secret::new(SECRET)
}

#[tokio::test]
async fn login_success() {
    let (server, client) = setup().await;
    mock("POST", "/1/users/login.json")
        .and(body_string("email=a%40example.org&password=pw"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"status": 1, "id": "uKey", "secret": SECRET, "request": "r1"}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let login = client.login("a@example.org", "pw", None).await.unwrap();
    assert_eq!(login.user_key, "uKey");
    assert_eq!(login.secret.expose(), SECRET);
    assert!(!format!("{login:?}").contains(SECRET));
}

#[tokio::test]
async fn login_requires_two_factor() {
    let (server, client) = setup().await;
    mock("POST", "/1/users/login.json")
        .and(body_string_contains("twofa=123456"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"status": 1, "id": "uKey", "secret": SECRET, "request": "r2"}),
            ),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    mock("POST", "/1/users/login.json")
        .respond_with(ResponseTemplate::new(412).set_body_json(
            json!({"status": 0, "errors": ["two-factor authentication required"], "request": "r3"}),
        ))
        .with_priority(2)
        .mount(&server)
        .await;

    let first = client.login("a@example.org", "pw", None).await;
    assert!(matches!(first, Err(Error::TwoFactorRequired)), "{first:?}");

    let second = client
        .login("a@example.org", "pw", Some("123456"))
        .await
        .unwrap();
    assert_eq!(second.secret.expose(), SECRET);
}

#[tokio::test]
async fn login_wrong_password() {
    let (server, client) = setup().await;
    mock("POST", "/1/users/login.json")
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"status": 0, "errors": ["invalid email and/or password"], "request": "r4"}),
        ))
        .mount(&server)
        .await;

    let err = client
        .login("a@example.org", "wrong", None)
        .await
        .unwrap_err();
    assert!(err.is_client_error());
    assert!(
        err.to_string().contains("invalid email and/or password"),
        "{err}"
    );
}

#[tokio::test]
async fn register_device_success() {
    let (server, client) = setup().await;
    mock("POST", "/1/devices.json")
        .and(body_string(format!("secret={SECRET}&name=my-desktop&os=O")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": 1, "id": "dev42", "request": "r5"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let name = DeviceName::new("my-desktop").unwrap();
    let id = client.register_device(&secret(), &name).await.unwrap();
    assert_eq!(id, DeviceId::new("dev42"));
}

#[tokio::test]
async fn register_device_name_taken() {
    let (server, client) = setup().await;
    mock("POST", "/1/devices.json")
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "status": 0,
            "errors": {"name": ["has already been taken"]},
            "request": "r6"
        })))
        .mount(&server)
        .await;

    let name = DeviceName::new("my-desktop").unwrap();
    match client.register_device(&secret(), &name).await {
        Err(Error::Api {
            errors, request, ..
        }) => {
            assert_eq!(errors, ["name has already been taken"]);
            assert_eq!(request.as_deref(), Some("r6"));
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn fetch_messages_from_fixture() {
    let (server, client) = setup().await;
    mock("GET", "/1/messages.json")
        .and(query_param("secret", SECRET))
        .and(query_param("device_id", "dev42"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(include_str!("fixtures/messages.json"), "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let fetched = client
        .fetch_messages(&secret(), &DeviceId::new("dev42"))
        .await
        .unwrap();
    assert!(fetched.malformed.is_empty());
    let msgs = &fetched.messages;
    assert_eq!(msgs.len(), 4);

    let plain = &msgs[0];
    assert_eq!(plain.id, MessageId(3));
    assert_eq!(plain.display_title(), "Backup");
    assert_eq!(plain.sound.as_deref(), Some("pushover"));
    assert!(!plain.html && !plain.acked && !plain.is_emergency());

    let untitled = &msgs[1];
    assert_eq!(untitled.title, None);
    assert_eq!(untitled.display_title(), "Cron");
    assert_eq!(untitled.priority, -1);
    assert_eq!(untitled.sound, None);

    let html = &msgs[2];
    assert!(html.html);
    assert_eq!(html.url.as_deref(), Some("https://example.org/release"));
    assert_eq!(html.url_title.as_deref(), Some("Release notes"));

    let emergency = &msgs[3];
    assert!(emergency.is_emergency());
    assert!(emergency.requires_persistent_display());
    assert!(emergency.can_acknowledge());
    assert_eq!(emergency.receipt, Some(Receipt::new("rcpt123abc")));

    assert_eq!(fetched.highest_id(), Some(MessageId(7)));
}

#[tokio::test]
async fn fetch_messages_empty() {
    let (server, client) = setup().await;
    mock("GET", "/1/messages.json")
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": 1, "messages": [], "request": "r7"})),
        )
        .mount(&server)
        .await;

    let fetched = client
        .fetch_messages(&secret(), &DeviceId::new("dev42"))
        .await
        .unwrap();
    assert!(fetched.is_empty());
    assert_eq!(fetched.highest_id(), None);
}

#[tokio::test]
async fn fetch_keeps_malformed_entries() {
    let (server, client) = setup().await;
    mock("GET", "/1/messages.json")
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": 1,
            "messages": [
                {"id": 1, "message": "fine", "app": "A", "aid": 1, "umid": 1, "date": 1},
                {"id": 2, "date": "not a number"}
            ],
            "request": "r10"
        })))
        .mount(&server)
        .await;

    let fetched = client
        .fetch_messages(&secret(), &DeviceId::new("dev42"))
        .await
        .unwrap();
    assert_eq!(fetched.messages.len(), 1);
    assert_eq!(fetched.malformed.len(), 1);
    assert_eq!(fetched.malformed[0].id, Some(MessageId(2)));
    assert_eq!(fetched.highest_id(), Some(MessageId(2)));
}

#[tokio::test]
async fn delete_up_to_highest() {
    let (server, client) = setup().await;
    mock("POST", "/1/devices/dev42/update_highest_message.json")
        .and(body_string(format!("secret={SECRET}&message=7")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"status": 1, "request": "r8"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    client
        .delete_up_to(&secret(), &DeviceId::new("dev42"), MessageId(7))
        .await
        .unwrap();
}

#[tokio::test]
async fn acknowledge_receipt() {
    let (server, client) = setup().await;
    mock("POST", "/1/receipts/rcpt123abc/acknowledge.json")
        .and(body_string(format!("secret={SECRET}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"status": 1, "request": "r9"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    client
        .acknowledge(&secret(), &Receipt::new("rcpt123abc"))
        .await
        .unwrap();
}

#[tokio::test]
async fn download_icon_and_sound() {
    let (server, client) = setup().await;
    mock("GET", "/icons/server.png")
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNG".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    mock("GET", "/sounds/siren.mp3")
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ID3".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let icon = client
        .download_icon(&ResourceName::new("server").unwrap())
        .await
        .unwrap();
    assert_eq!(icon, b"\x89PNG");
    let sound = client
        .download_sound(&ResourceName::new("siren").unwrap())
        .await
        .unwrap();
    assert_eq!(sound, b"ID3");

    let missing = client
        .download_icon(&ResourceName::new("nope").unwrap())
        .await;
    assert!(matches!(missing, Err(Error::Api { http_status, .. }) if http_status == 404));
}

#[tokio::test]
async fn server_error_without_json_body() {
    let (server, client) = setup().await;
    mock("GET", "/1/messages.json")
        .respond_with(ResponseTemplate::new(503).set_body_string("<html>down</html>"))
        .mount(&server)
        .await;

    let err = client
        .fetch_messages(&secret(), &DeviceId::new("dev42"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Api { http_status, .. } if *http_status == 503),
        "{err:?}"
    );
    assert!(!err.is_client_error());
}

#[tokio::test]
async fn transport_errors_do_not_leak_secret() {
    // Nothing listens on this port: connection refused.
    let client = Client::with_base_url(UA, "http://127.0.0.1:9/").unwrap();
    let err = client
        .fetch_messages(&secret(), &DeviceId::new("dev42"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Http(_)), "{err:?}");
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains(SECRET), "secret leaked: {shown}");
}

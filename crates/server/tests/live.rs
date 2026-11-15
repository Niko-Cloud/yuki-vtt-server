//! The live channel, over a real socket.

use futures_util::StreamExt;
use serde_json::json;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use yuki_vtt_server::state::{AppState, Config};
use yuki_vtt_server::{app, db, ids};

const KEY: &str = "the-dms-key";
const SITE: &str = "https://yuki.test";

async fn serve() -> String {
    let config = Config {
        owner_key_hash: ids::hash(KEY),
        website_secret: None,
        allowed_origins: vec![SITE.into()],
        // Its own folder, kept until the test run ends.
        data_dir: tempfile::tempdir().unwrap().keep(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app(AppState::new(db::in_memory().unwrap(), config)),
        )
        .await
        .unwrap()
    });
    format!("127.0.0.1:{}", address.port())
}

async fn new_table(client: &reqwest_like::Client, host: &str) -> String {
    client
        .post(
            &format!("http://{host}/api/tables"),
            json!({ "name": "Live" }),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

// The DM's pages open the channel with a media pass, never the DM's key.
async fn media_pass(client: &reqwest_like::Client, host: &str, table: &str) -> String {
    client
        .post(
            &format!("http://{host}/api/tables/{table}/media-token"),
            json!({}),
        )
        .await["pass"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn tells_everyone_at_the_table_when_something_changes() {
    let host = serve().await;
    let client = reqwest_like::Client::new(KEY);
    let table = new_table(&client, &host).await;
    let pass = media_pass(&client, &host, &table).await;

    let mut request = format!("ws://{host}/api/tables/{table}/events?pass={pass}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", SITE.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();

    client
        .post(
            &format!("http://{host}/api/tables/{table}/roll"),
            json!({ "label": "Luck", "expression": "1d20" }),
        )
        .await;
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("an event in time")
        .unwrap()
        .unwrap();
    assert_eq!(
        message.into_text().unwrap().as_str(),
        r#"{"type":"change"}"#
    );
}

#[tokio::test]
async fn refuses_other_websites_and_strangers() {
    let host = serve().await;
    let client = reqwest_like::Client::new(KEY);
    let table = new_table(&client, &host).await;
    let pass = media_pass(&client, &host, &table).await;

    let mut request = format!("ws://{host}/api/tables/{table}/events?pass={pass}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "https://evil.test".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(request).await.is_err());

    let stranger = format!("ws://{host}/api/tables/{table}/events?pass=.1.guess")
        .into_client_request()
        .unwrap();
    assert!(tokio_tungstenite::connect_async(stranger).await.is_err());

    // The DM's key in the address is no longer taken.
    let mut keyed = format!("ws://{host}/api/tables/{table}/events?key={KEY}")
        .into_client_request()
        .unwrap();
    keyed.headers_mut().insert("origin", SITE.parse().unwrap());
    assert!(tokio_tungstenite::connect_async(keyed).await.is_err());
}

// Just enough of an HTTP client for these tests, on the same stack as the server.
mod reqwest_like {
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub struct Client {
        key: String,
    }

    impl Client {
        pub fn new(key: &str) -> Self {
            Self { key: key.into() }
        }

        pub async fn post(&self, url: &str, body: Value) -> Value {
            let rest = url.strip_prefix("http://").unwrap();
            let (host, path) = rest.split_once('/').unwrap();
            let body = body.to_string();
            let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
            let request = format!(
                "POST /{path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                self.key,
                body.len()
            );
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            let (_, json) = response.split_once("\r\n\r\n").unwrap();
            serde_json::from_str(json).unwrap_or(Value::Null)
        }
    }
}

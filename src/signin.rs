use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use url::Url;
use webkit6::prelude::*;
use webkit6::{glib, soup};

// Signing in happens in the system browser, not in the app: the app opens
// Reevun ID's id.reevun.app/app?port=…&challenge=…&state=… and listens there.
// 127.0.0.1. There the person signs in (or already is) and confirms; Reevun
// ID sends a one-time code back here. The app trades it with its private
// PKCE verifier for the session, so the session token never appears in a
// browser address. An unfinished sign-in stops listening after this long.
const TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub struct Pending {
    pub url: String,
    stop: Arc<AtomicBool>,
}

impl Pending {
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel();
    }
}

// Starts listening; `signed_in` gets the session token, on the main thread.
pub fn start(signed_in: impl FnOnce(String) + 'static) -> std::io::Result<Pending> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let mut verifier_bytes = [0u8; 32];
    getrandom::fill(&mut verifier_bytes).map_err(std::io::Error::other)?;
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));

    let mut url =
        Url::parse(&format!("{}/app", crate::site::id_url())).expect("Reevun ID's address");
    url.query_pairs_mut()
        .append_pair("port", &port.to_string())
        .append_pair("challenge", &challenge)
        .append_pair("state", &state);
    let stop = Arc::new(AtomicBool::new(false));

    let (send, receive) = async_channel::bounded::<(String, String)>(1);
    let stopped = Arc::clone(&stop);
    let redeem_verifier = verifier.clone();
    thread::spawn(move || {
        let started = Instant::now();
        while !stopped.load(Ordering::Relaxed) && started.elapsed() < TIMEOUT {
            let Ok((stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(100));
                continue;
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut line = String::new();
            let read = BufReader::new(&stream).take(8193).read_line(&mut line);
            let mut stream = stream;
            match read
                .ok()
                .filter(|count| *count <= 8192)
                .and_then(|_| code_from(&line, &state))
            {
                Some(code) => {
                    let done = format!("{}/app/done", crate::site::id_url());
                    let _ = write!(
                        stream,
                        "HTTP/1.1 302 Found\r\nLocation: {done}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = send.send_blocking((code, redeem_verifier.clone()));
                    return;
                }
                None => {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                }
            }
        }
    });
    glib::spawn_future_local(async move {
        if let Ok((code, verifier)) = receive.recv().await
            && let Some(token) = redeem(&code, &verifier).await
        {
            signed_in(token)
        }
    });
    Ok(Pending {
        url: url.into(),
        stop,
    })
}

// "GET /callback?state=…&code=… HTTP/1.1" with this sign-in's state.
fn code_from(request_line: &str, state: &str) -> Option<String> {
    let target = request_line.strip_prefix("GET ")?.split(' ').next()?;
    let url = Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    if url.path() != "/callback" {
        return None;
    }
    let value = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    (value("state")? == state)
        .then(|| value("code"))
        .flatten()
        .filter(|code| !code.is_empty())
}

#[derive(Deserialize)]
struct Redeemed {
    token: String,
}

async fn redeem(code: &str, verifier: &str) -> Option<String> {
    let session = soup::Session::new();
    session.set_timeout(15);
    let message =
        soup::Message::new("POST", &format!("{}/v1/auth/code", crate::site::api_url())).ok()?;
    let body =
        serde_json::to_vec(&json!({ "code": code, "client": "app", "verifier": verifier })).ok()?;
    let bytes = glib::Bytes::from_owned(body);
    message.set_request_body_from_bytes(Some("application/json"), Some(&bytes));
    let response = session
        .send_and_read_future(&message, glib::Priority::DEFAULT)
        .await
        .ok()?;
    if message.status() != soup::Status::Ok {
        return None;
    }
    serde_json::from_slice::<Redeemed>(&response)
        .ok()
        .map(|body| body.token)
        .filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::code_from;

    #[test]
    fn only_this_sign_in_and_a_token() {
        assert_eq!(
            code_from("GET /callback?state=abc&code=c%2B1 HTTP/1.1\r\n", "abc").as_deref(),
            Some("c+1")
        );
        assert_eq!(
            code_from("GET /callback?state=xyz&code=c HTTP/1.1\r\n", "abc"),
            None
        );
        assert_eq!(
            code_from("GET /callback?state=abc HTTP/1.1\r\n", "abc"),
            None
        );
        assert_eq!(
            code_from("GET /other?state=abc&code=c HTTP/1.1\r\n", "abc"),
            None
        );
        assert_eq!(
            code_from("POST /callback?state=abc&code=c HTTP/1.1\r\n", "abc"),
            None
        );
    }
}

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use url::Url;
use webkit6::glib;

// Signing in happens in the system browser, not in the app: the app opens
// Reevun ID's id.reevun.app/app?port=…&state=… and listens on that port of
// 127.0.0.1. There the person signs in (or already is) and confirms; Reevun
// ID sends the browser back here with a session of the app's own, which
// becomes the app's session cookie - the same one the website reads.
// An unfinished sign-in stops listening after this long.
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

    let mut url =
        Url::parse(&format!("{}/app", crate::site::id_url())).expect("Reevun ID's address");
    url.query_pairs_mut()
        .append_pair("port", &port.to_string())
        .append_pair("state", &state);
    let stop = Arc::new(AtomicBool::new(false));

    let (send, receive) = async_channel::bounded::<String>(1);
    let stopped = Arc::clone(&stop);
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
            let _ = BufReader::new(&stream).read_line(&mut line);
            let mut stream = stream;
            match token_from(&line, &state) {
                Some(token) => {
                    let done = format!("{}/app/done", crate::site::id_url());
                    let _ = write!(
                        stream,
                        "HTTP/1.1 302 Found\r\nLocation: {done}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = send.send_blocking(token);
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
        if let Ok(token) = receive.recv().await {
            signed_in(token);
        }
    });
    Ok(Pending {
        url: url.into(),
        stop,
    })
}

// "GET /callback?state=…&token=… HTTP/1.1" with this sign-in's state.
fn token_from(request_line: &str, state: &str) -> Option<String> {
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
        .then(|| value("token"))
        .flatten()
        .filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::token_from;

    #[test]
    fn only_this_sign_in_and_a_token() {
        assert_eq!(
            token_from("GET /callback?state=abc&token=t%2B1 HTTP/1.1\r\n", "abc").as_deref(),
            Some("t+1")
        );
        assert_eq!(
            token_from("GET /callback?state=xyz&token=t HTTP/1.1\r\n", "abc"),
            None
        );
        assert_eq!(
            token_from("GET /callback?state=abc HTTP/1.1\r\n", "abc"),
            None
        );
        assert_eq!(
            token_from("GET /other?state=abc&token=t HTTP/1.1\r\n", "abc"),
            None
        );
        assert_eq!(
            token_from("POST /callback?state=abc&token=t HTTP/1.1\r\n", "abc"),
            None
        );
    }
}

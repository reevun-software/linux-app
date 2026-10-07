use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use serde::Deserialize;
use serde_json::json;
use webkit6::prelude::*;
use webkit6::{gio, glib, soup};

use crate::screens;

// Updates come from the linux-app GitHub releases. Every start goes through
// a small launch window that asks for the newest release and, when it's
// newer, downloads its package (.deb or .rpm, whichever installed this
// app), checks it against the release's checksum, installs it with the
// system's own permission prompt and starts the new version. No newer
// release, no answer within CHECK_TIMEOUT, a declined prompt, or an app not
// installed from a package (a build run from source): it just opens.
const RELEASE: &str = "https://api.github.com/repos/reevun-software/linux-app/releases/latest";
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy)]
enum Package {
    Deb,
    Rpm,
}

impl Package {
    fn installed() -> Option<Package> {
        if Path::new("/var/lib/dpkg/info/reevun.list").exists() {
            return Some(Package::Deb);
        }
        let rpm = Command::new("rpm").args(["-q", "reevun"]).output();
        rpm.is_ok_and(|out| out.status.success())
            .then_some(Package::Rpm)
    }

    fn asset(self) -> &'static str {
        match self {
            Package::Deb => "Reevun.deb",
            Package::Rpm => "Reevun.rpm",
        }
    }

    fn install(self, file: &Path) -> Command {
        let mut command = Command::new("pkexec");
        match self {
            Package::Deb => command.args(["dpkg", "-i"]),
            Package::Rpm => command.args(["rpm", "-U", "--force"]),
        };
        command.arg(file);
        command
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

fn numbers(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

pub fn on_launch(open: impl FnOnce() + 'static) {
    let Some(package) = Package::installed() else {
        return open();
    };
    let window = gtk::Window::builder()
        .title("Reevun")
        .decorated(false)
        .resizable(false)
        .default_width(320)
        .default_height(360)
        .build();
    let launch = screens::view("launch", |_, message| {
        (message.method == "info").then(screens::info)
    });
    window.set_child(Some(&launch));
    window.present();

    let open = Rc::new(RefCell::new(Some(open)));
    glib::spawn_future_local(async move {
        let report = |status: serde_json::Value| screens::emit(&launch, "updateStatus", status);
        match update(package, &report).await {
            Ok(true) => {
                // The new version starts once this one has quit.
                let _ = Command::new("sh")
                    .args(["-c", "sleep 1; exec reevun"])
                    .spawn();
                if let Some(app) = gio::Application::default() {
                    app.quit();
                }
            }
            Ok(false) | Err(_) => {
                if let Some(open) = open.borrow_mut().take() {
                    open();
                }
                window.destroy();
            }
        }
    });
}

// Whether a newer version was installed.
async fn update(
    package: Package,
    report: &impl Fn(serde_json::Value),
) -> Result<bool, Box<dyn std::error::Error>> {
    report(json!({ "phase": "checking" }));
    let session = soup::Session::new();
    session.set_timeout(CHECK_TIMEOUT.as_secs() as u32);
    session.set_user_agent(&format!("Reevun/{}", crate::VERSION));
    let message = soup::Message::new("GET", RELEASE)?;
    let body = session
        .send_and_read_future(&message, glib::Priority::DEFAULT)
        .await?;
    let release: Release = serde_json::from_slice(&body)?;
    if numbers(&release.tag_name) <= numbers(crate::VERSION) {
        return Ok(false);
    }
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == package.asset())
        .ok_or("no package")?;
    let expected = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .ok_or("no checksum")?;

    report(json!({ "phase": "downloading", "percent": 0 }));
    session.set_timeout(60);
    let message = soup::Message::new("GET", &asset.browser_download_url)?;
    let stream = session
        .send_future(&message, glib::Priority::DEFAULT)
        .await?;
    let total = message
        .response_headers()
        .map(|headers| headers.content_length())
        .unwrap_or(0)
        .max(1) as f64;
    let mut data = Vec::new();
    let mut checksum = glib::Checksum::new(glib::ChecksumType::Sha256).ok_or("no sha256")?;
    loop {
        let chunk = stream
            .read_bytes_future(1 << 16, glib::Priority::DEFAULT)
            .await?;
        if chunk.is_empty() {
            break;
        }
        checksum.update(&chunk);
        data.extend_from_slice(&chunk);
        report(
            json!({ "phase": "downloading", "percent": ((data.len() as f64 / total) * 100.0).min(100.0).floor() }),
        );
    }
    if checksum.string().as_deref() != Some(expected) {
        return Err("checksum".into());
    }
    let file: PathBuf = glib::user_cache_dir().join("reevun").join(package.asset());
    std::fs::create_dir_all(file.parent().ok_or("cache")?)?;
    std::fs::write(&file, &data)?;

    report(json!({ "phase": "installing" }));
    let mut install = package.install(&file);
    let status = gio::spawn_blocking(move || install.status()).await;
    let _ = std::fs::remove_file(&file);
    Ok(status.map_err(|_| "install")??.success())
}

#[cfg(test)]
mod tests {
    use super::numbers;

    #[test]
    fn newer_by_number_not_by_text() {
        assert!(numbers("v1.0.10") > numbers("1.0.9"));
        assert!(numbers("v1.0.9") <= numbers("1.0.9"));
        assert!(numbers("v2.0.0") > numbers("1.9.99"));
    }
}

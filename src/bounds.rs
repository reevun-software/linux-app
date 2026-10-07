use std::fs;
use std::path::PathBuf;

use gtk::prelude::*;
use serde::{Deserialize, Serialize};
use webkit6::glib;

// The window reopens at the size the person left it (and maximized if it
// was). Where it stands is up to the system on Linux.
#[derive(Serialize, Deserialize)]
struct Bounds {
    width: i32,
    height: i32,
    maximized: bool,
}

fn file() -> PathBuf {
    glib::user_config_dir().join("reevun").join("window.json")
}

pub fn restore(window: &gtk::ApplicationWindow) {
    let saved = fs::read(file())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Bounds>(&bytes).ok());
    let bounds = saved.unwrap_or(Bounds {
        width: 1360,
        height: 860,
        maximized: false,
    });
    window.set_default_size(bounds.width.max(960), bounds.height.max(620));
    if bounds.maximized {
        window.maximize();
    }
}

pub fn save(window: &gtk::ApplicationWindow) {
    let (width, height) = window.default_size();
    let bounds = Bounds {
        width,
        height,
        maximized: window.is_maximized(),
    };
    let path = file();
    if let (Some(dir), Ok(json)) = (path.parent(), serde_json::to_vec(&bounds)) {
        let _ = fs::create_dir_all(dir).and_then(|_| fs::write(&path, json));
    }
}

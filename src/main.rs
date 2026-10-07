// Reevun for Linux: reevun.app in a window of its own (GTK 4 and WebKitGTK),
// with the app's own screens - title strip, loading and offline screens,
// update window - from reevun-software/app-core, the same in every Reevun
// app.
mod bounds;
mod screens;
mod signin;
mod site;
mod update;
mod window;

use gtk::prelude::*;
use webkit6::{gio, glib};

pub const APP_ID: &str = "app.reevun.desktop";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> glib::ExitCode {
    // The name the desktop and its .desktop file know the app by.
    glib::set_prgname(Some(APP_ID));
    // One app at a time: starting it again brings the open window forward.
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::empty())
        .build();
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        // The first start checks for an update before the window opens.
        let hold = app.hold();
        let app = app.clone();
        update::on_launch(move || {
            window::open(&app);
            drop(hold);
        });
    });
    app.run()
}

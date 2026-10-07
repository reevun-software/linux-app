use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use serde_json::{Value, json};
use webkit6::prelude::*;
use webkit6::{
    CookiePersistentStorage, LoadEvent, NetworkError, NetworkSession, PolicyError, Settings,
    WebContext, WebView, gdk, glib, soup,
};

use crate::screens::{self, Message};
use crate::{bounds, signin, site};

// The window: the app's title strip on top, the site under it, and the
// loading screen over the site until it has loaded - or saying it can't
// (offline), or waiting for the sign-in in the browser.
const STRIP_HEIGHT: i32 = 36;
// The loading screen stays at least this long, so it fades instead of
// flashing; the fade itself takes FADE.
const MIN_LOADING: Duration = Duration::from_millis(900);
const FADE: Duration = Duration::from_millis(400);
const SESSION_COOKIE: &str = "reevun_session";
const SESSION_DAYS: i32 = 60;

#[derive(Clone, Copy, PartialEq)]
enum SiteState {
    Loading,
    Offline,
    Browser,
    Ready,
}

impl SiteState {
    fn name(self) -> &'static str {
        match self {
            SiteState::Loading => "loading",
            SiteState::Offline => "offline",
            SiteState::Browser => "browser",
            SiteState::Ready => "ready",
        }
    }
}

#[derive(Clone)]
struct Press {
    device: gdk::Device,
    button: u32,
    x: f64,
    y: f64,
    time: u32,
}

struct Shell {
    window: gtk::ApplicationWindow,
    overlay: gtk::Overlay,
    site: WebView,
    strip: WebView,
    loading: RefCell<Option<WebView>>,
    // The loading screen fading out, if one is.
    hiding: RefCell<Option<WebView>>,
    shown_at: Cell<Instant>,
    state: Cell<SiteState>,
    failed: Cell<bool>,
    sign_in: RefCell<Option<signin::Pending>>,
    // The last press on the title strip, for moving the window by it.
    press: RefCell<Option<Press>>,
}

pub fn open(app: &gtk::Application) {
    let shell = Rc::new_cyclic(|weak: &Weak<Shell>| {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Reevun")
            .build();
        window.set_size_request(960, 620);
        bounds::restore(&window);

        let on_strip = weak.clone();
        let strip = screens::view("titlebar", move |_, message| {
            on_strip.upgrade()?.strip_message(message)
        });
        strip.set_size_request(-1, STRIP_HEIGHT);
        window.set_titlebar(Some(&strip));
        // No title bar look from the system theme: the strip draws itself.
        strip.remove_css_class("titlebar");

        let site = site_view();
        let overlay = gtk::Overlay::builder().child(&site).build();
        window.set_child(Some(&overlay));

        Shell {
            window,
            overlay,
            site,
            strip,
            loading: RefCell::new(None),
            hiding: RefCell::new(None),
            shown_at: Cell::new(Instant::now()),
            state: Cell::new(SiteState::Loading),
            failed: Cell::new(false),
            sign_in: RefCell::new(None),
            press: RefCell::new(None),
        }
    });
    shell.show_overlay(SiteState::Loading);
    shell.watch_site();
    shell.watch_strip();
    shell.dashboard();

    let closing = Rc::clone(&shell);
    shell.window.connect_close_request(move |window| {
        bounds::save(window);
        closing.sign_in.borrow_mut().take();
        glib::Propagation::Proceed
    });
    shell.window.present();
}

// The site's web view: its own lasting session (cookies kept between
// starts), spell checking, and the app's mark in the user agent.
fn site_view() -> WebView {
    let data = glib::user_data_dir().join("reevun");
    let cache = glib::user_cache_dir().join("reevun");
    let session = NetworkSession::new(data.to_str(), cache.to_str());
    if let Some(cookies) = session.cookie_manager() {
        cookies.set_persistent_storage(
            &data.join("cookies.sqlite").to_string_lossy(),
            CookiePersistentStorage::Sqlite,
        );
    }
    let context = WebContext::new();
    context.set_spell_checking_enabled(true);
    let languages: Vec<String> = glib::language_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    context.set_spell_checking_languages(&languages.iter().map(String::as_str).collect::<Vec<_>>());

    let settings = Settings::new();
    let agent = settings
        .user_agent()
        .map(|agent| agent.to_string())
        .unwrap_or_default();
    settings.set_user_agent(Some(&format!(
        "{agent} ReevunApp/{} (linux)",
        crate::VERSION
    )));
    let view = WebView::builder()
        .network_session(&session)
        .web_context(&context)
        .settings(&settings)
        .build();
    view.set_background_color(&gdk::RGBA::WHITE);
    view.set_vexpand(true);
    view
}

impl Shell {
    fn dashboard(&self) {
        self.site
            .load_uri(&format!("{}/dashboard", site::site_url()));
    }

    fn report(&self, state: SiteState) {
        self.state.set(state);
        if let Some(loading) = self.loading.borrow().as_ref() {
            screens::emit(loading, "siteState", json!(state.name()));
        }
    }

    // The screen over the site, in a given state (made again if it's gone
    // or going).
    fn show_overlay(self: &Rc<Self>, state: SiteState) {
        let current = self.loading.borrow().clone();
        if current.is_none() || current == *self.hiding.borrow() {
            let weak = Rc::downgrade(self);
            let view = screens::view("loading", move |_, message| {
                weak.upgrade()?.loading_message(message)
            });
            self.overlay.add_overlay(&view);
            *self.loading.borrow_mut() = Some(view);
            self.shown_at.set(Instant::now());
        }
        self.report(state);
    }

    // The site has loaded: the screen fades out (its page does that on
    // "ready"), then goes.
    fn show_site(self: &Rc<Self>) {
        let Some(overlay) = self.loading.borrow().clone() else {
            return;
        };
        if self.hiding.borrow().as_ref() == Some(&overlay) {
            return;
        }
        *self.hiding.borrow_mut() = Some(overlay.clone());
        let wait = MIN_LOADING.saturating_sub(self.shown_at.get().elapsed());
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(wait, move || {
            let Some(shell) = weak.upgrade() else { return };
            shell.report(SiteState::Ready);
            glib::timeout_add_local_once(FADE, move || {
                shell.overlay.remove_overlay(&overlay);
                if shell.loading.borrow().as_ref() == Some(&overlay) {
                    shell.loading.borrow_mut().take();
                }
                if shell.hiding.borrow().as_ref() == Some(&overlay) {
                    shell.hiding.borrow_mut().take();
                }
            });
        });
    }

    fn watch_site(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        site::guard(
            &self.site,
            Rc::new(move || {
                if let Some(shell) = weak.upgrade() {
                    shell.show_overlay(SiteState::Browser);
                    shell.sign_in_in_browser();
                }
            }),
        );
        // A page that failed (offline, the site down) keeps the screen up
        // in its offline state until a load succeeds; a load stopped for
        // the browser (above) doesn't take the screen away either.
        let weak = Rc::downgrade(self);
        self.site.connect_load_changed(move |site, event| {
            let Some(shell) = weak.upgrade() else { return };
            match event {
                LoadEvent::Started => shell.failed.set(false),
                LoadEvent::Redirected if !site.uri().is_some_and(|url| site::stays(&url)) => {
                    shell.failed.set(true)
                }
                LoadEvent::Finished
                    if !shell.failed.get() && shell.state.get() != SiteState::Ready =>
                {
                    shell.show_site()
                }
                _ => {}
            }
        });
        let weak = Rc::downgrade(self);
        self.site.connect_load_failed(move |_, _, _, error| {
            let Some(shell) = weak.upgrade() else {
                return false;
            };
            shell.failed.set(true);
            // A load replaced by another one, or stopped by the app: not
            // offline.
            if error.matches(NetworkError::Cancelled)
                || error.matches(PolicyError::FrameLoadInterruptedByPolicyChange)
            {
                return false;
            }
            // The offline screen instead of WebKit's own error page.
            shell.show_overlay(SiteState::Offline);
            true
        });
    }

    // While the browser sign-in is open the app waits on its own screen;
    // signed in, the dashboard loads with the new session.
    fn sign_in_in_browser(self: &Rc<Self>) {
        if let Some(pending) = self.sign_in.borrow().as_ref() {
            site::open_outside(Some(self.window.upcast_ref()), &pending.url);
            return;
        }
        let weak = Rc::downgrade(self);
        let started = signin::start(move |token| {
            if let Some(shell) = weak.upgrade() {
                shell.sign_in.borrow_mut().take();
                shell.signed_in(&token);
            }
        });
        if let Ok(pending) = started {
            site::open_outside(Some(self.window.upcast_ref()), &pending.url);
            *self.sign_in.borrow_mut() = Some(pending);
        }
    }

    fn signed_in(self: &Rc<Self>, token: &str) {
        let host = url::Url::parse(&site::site_url())
            .ok()
            .and_then(|url| url.host_str().map(String::from))
            .unwrap_or_default();
        let mut cookie = soup::Cookie::new(
            SESSION_COOKIE,
            token,
            &host,
            "/",
            SESSION_DAYS * 24 * 60 * 60,
        );
        cookie.set_secure(true);
        cookie.set_http_only(true);
        cookie.set_same_site_policy(soup::SameSitePolicy::Lax);
        let Some(cookies) = self
            .site
            .network_session()
            .and_then(|session| session.cookie_manager())
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        cookies.add_cookie(&cookie, None::<&gtk::gio::Cancellable>, move |_| {
            if let Some(shell) = weak.upgrade() {
                shell.report(SiteState::Loading);
                shell.dashboard();
                shell.window.present();
            }
        });
    }

    fn loading_message(self: &Rc<Self>, message: &Message) -> Option<Value> {
        match message.method.as_str() {
            "info" => return Some(screens::info()),
            "siteState" => return Some(json!(self.state.get().name())),
            "retry" => {
                self.report(SiteState::Loading);
                self.dashboard();
            }
            "reopenSignIn" => self.sign_in_in_browser(),
            "cancelSignIn" => {
                self.sign_in.borrow_mut().take();
                self.show_site();
            }
            _ => {}
        }
        None
    }

    // The strip's own window buttons (it shows "restore" while maximized),
    // and moving the window by the strip.
    fn strip_message(&self, message: &Message) -> Option<Value> {
        let window = &self.window;
        match (message.method.as_str(), message.arg()) {
            ("info", _) => return Some(screens::info()),
            ("isMaximized", _) => return Some(json!(window.is_maximized())),
            ("windowControl", "minimize") => window.minimize(),
            ("windowControl", "maximize") if window.is_maximized() => window.unmaximize(),
            ("windowControl", "maximize") => window.maximize(),
            ("windowControl", "close") => window.close(),
            ("windowControl", "drag") => self.move_window(),
            _ => {}
        }
        None
    }

    fn watch_strip(self: &Rc<Self>) {
        let press = gtk::GestureClick::builder()
            .button(0)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let weak = Rc::downgrade(self);
        press.connect_pressed(move |gesture, _, x, y| {
            let (Some(shell), Some(device)) = (weak.upgrade(), gesture.current_event_device())
            else {
                return;
            };
            *shell.press.borrow_mut() = Some(Press {
                device,
                button: gesture.current_button(),
                x,
                y,
                time: gesture.current_event_time(),
            });
        });
        self.strip.add_controller(press);

        let weak = Rc::downgrade(self);
        self.window.connect_maximized_notify(move |window| {
            if let Some(shell) = weak.upgrade() {
                screens::emit(&shell.strip, "maximized", json!(window.is_maximized()));
            }
        });
    }

    fn move_window(&self) {
        let Some(Press {
            device,
            button,
            x,
            y,
            time,
        }) = self.press.borrow().clone()
        else {
            return;
        };
        let Some(toplevel) = self.window.surface().and_downcast::<gdk::Toplevel>() else {
            return;
        };
        let Some(point) = self
            .strip
            .compute_point(&self.window, &gtk::graphene::Point::new(x as f32, y as f32))
        else {
            return;
        };
        let (dx, dy) = self.window.surface_transform();
        toplevel.begin_move(
            &device,
            button as i32,
            point.x() as f64 + dx,
            point.y() as f64 + dy,
            time,
        );
    }
}

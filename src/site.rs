use std::rc::Rc;

use gtk::prelude::*;
use url::Url;
use webkit6::prelude::*;
use webkit6::{
    LoadEvent, NavigationPolicyDecision, NavigationType, PolicyDecisionType, WebView, gio,
};

// The site, marked as the app in its user agent (it then opens on the
// dashboard and keeps the profile in the bottom-left corner). Pages that
// open inside the app: the site itself and the other Reevun sites, and
// Discord's bot invite. Reevun ID (signing in, account settings) opens in
// the system browser, as does anything else.
pub fn site_url() -> String {
    std::env::var("REEVUN_SITE_URL")
        .unwrap_or_else(|_| "https://reevun.app".into())
        .trim_end_matches('/')
        .into()
}

pub fn id_url() -> String {
    std::env::var("REEVUN_ID_URL")
        .unwrap_or_else(|_| "https://id.reevun.app".into())
        .trim_end_matches('/')
        .into()
}

fn in_app_host(host: &str) -> bool {
    host == "reevun.app"
        || host.ends_with(".reevun.app")
        || host == "discord.com"
        || host == "www.discord.com"
}

fn is_id_page(url: &Url) -> bool {
    Url::parse(&id_url()).is_ok_and(|id| id.origin() == url.origin())
}

// The site's "Sign in" (the API's single sign-on for reevun.app): in the app
// that is the sign-in in the browser instead.
pub fn is_sign_in(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| url.scheme() == "https" && url.path() == "/v1/auth/app/sso")
}

pub fn stays(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.host_str().is_some_and(in_app_host)
        && !is_id_page(&parsed)
        && !is_sign_in(url)
}

pub fn open_outside(window: Option<&gtk::Window>, url: &str) {
    if url.starts_with("https:") || url.starts_with("mailto:") {
        gtk::UriLauncher::new(url).launch(window, None::<&gio::Cancellable>, |_| {});
    }
}

type Leave = Rc<dyn Fn(&WebView, &str)>;

// Reevun pages stay (Discord's bot invite as a popup window, as on the
// website); signing in goes to `sign_in`; Reevun ID's pages and other links
// go to the browser - links, new windows, the site sending the page to
// signing in or Reevun ID, and redirects elsewhere. What the page embeds
// (frames) is left alone.
pub fn guard(view: &WebView, sign_in: Rc<dyn Fn()>) {
    let leave: Leave = {
        let sign_in = Rc::clone(&sign_in);
        Rc::new(move |view, url| {
            if is_sign_in(url) {
                sign_in();
            } else {
                open_outside(view.root().and_downcast::<gtk::Window>().as_ref(), url);
            }
        })
    };
    let goes = |url: &str| {
        !(url.starts_with("about:")
            || url.starts_with("blob:")
            || url.starts_with("data:")
            || stays(url))
    };

    // Clicked links and sent forms (in the page or a frame), new windows, and
    // anything going to signing in or Reevun ID (which no frame shows).
    let on_leave = Rc::clone(&leave);
    view.connect_decide_policy(move |view, decision, kind| {
        let Some(action) = decision
            .downcast_ref::<NavigationPolicyDecision>()
            .and_then(|navigation| navigation.navigation_action())
        else {
            return false;
        };
        let clicked = matches!(
            action.navigation_type(),
            NavigationType::LinkClicked
                | NavigationType::FormSubmitted
                | NavigationType::FormResubmitted
        );
        let Some(url) = action.request().and_then(|request| request.uri()) else {
            return false;
        };
        let signing_in = is_sign_in(&url) || Url::parse(&url).is_ok_and(|url| is_id_page(&url));
        if !(kind == PolicyDecisionType::NewWindowAction || clicked || signing_in) || !goes(&url) {
            return false;
        }
        decision.ignore();
        on_leave(view, &url);
        true
    });
    // The page redirected elsewhere.
    view.connect_load_changed(move |view, event| {
        if event != LoadEvent::Redirected {
            return;
        }
        let Some(url) = view.uri() else { return };
        if goes(&url) {
            view.stop_loading();
            leave(view, &url);
        }
    });

    view.connect_create(move |view, _| {
        let popup = WebView::builder().related_view(view).build();
        if let Some(settings) = WebViewExt::settings(view) {
            popup.set_settings(&settings);
        }
        guard(&popup, Rc::clone(&sign_in));
        let window = gtk::Window::builder()
            .default_width(500)
            .default_height(760)
            .child(&popup)
            .build();
        if let Some(parent) = view.root().and_downcast::<gtk::Window>() {
            window.set_transient_for(Some(&parent));
        }
        popup.connect_ready_to_show(move |_| window.present());
        popup.connect_close(|popup| {
            if let Some(window) = popup.root().and_downcast::<gtk::Window>() {
                window.destroy();
            }
        });
        Some(popup.upcast())
    });
}

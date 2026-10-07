use std::rc::Rc;

use gtk::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use webkit6::prelude::*;
use webkit6::{NetworkSession, UserContentManager, WebContext, WebView, gio, glib};

// The app's own screens (reevun-software/app-core, one self-contained page
// built into screens/index.html), served to the app's own web views at
// reevun-app://screens/ and talking to the app over the "reevun" message
// handler; the site's web view has neither.
const PAGE: &str = include_str!("../screens/index.html");
const SCHEME: &str = "reevun-app";

#[derive(Deserialize)]
pub struct Message {
    id: Option<u64>,
    pub method: String,
    #[serde(default)]
    pub args: Vec<Value>,
}

impl Message {
    pub fn arg(&self) -> &str {
        self.args
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

thread_local! {
    static CONTEXT: WebContext = {
        let context = WebContext::new();
        context.register_uri_scheme(SCHEME, |request| {
            let bytes = glib::Bytes::from_static(PAGE.as_bytes());
            let stream = gio::MemoryInputStream::from_bytes(&bytes);
            request.finish(&stream, PAGE.len() as i64, Some("text/html"));
        });
        context
    };
    static SESSION: NetworkSession = NetworkSession::new_ephemeral();
}

// A web view showing one of the screens: "launch", "titlebar" or "loading".
// Each message from it goes to `handle`, whose answer (if any) goes back.
pub fn view(page: &str, handle: impl Fn(&WebView, &Message) -> Option<Value> + 'static) -> WebView {
    let content = UserContentManager::new();
    content.register_script_message_handler("reevun", None);
    let view = CONTEXT.with(|context| {
        SESSION.with(|session| {
            WebView::builder()
                .web_context(context)
                .network_session(session)
                .user_content_manager(&content)
                .build()
        })
    });
    view.set_background_color(&gtk::gdk::RGBA::WHITE);
    let handle = Rc::new(handle);
    let weak = view.downgrade();
    content.connect_script_message_received(Some("reevun"), move |_, value| {
        let Some(view) = weak.upgrade() else { return };
        let Ok(message) = serde_json::from_str::<Message>(value.to_str().as_str()) else {
            return;
        };
        let result = handle(&view, &message).unwrap_or(Value::Null);
        if let Some(id) = message.id {
            send(&view, json!({ "id": id, "result": result }));
        }
    });
    view.load_uri(&format!("{SCHEME}://screens/#{page}"));
    view
}

// An event for a screen: "siteState", "maximized" or "updateStatus".
pub fn emit(view: &WebView, event: &str, data: Value) {
    send(view, json!({ "event": event, "data": data }));
}

fn send(view: &WebView, message: Value) {
    let script = format!("window.reevunNative && window.reevunNative.receive({message})");
    view.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
}

// What every screen asks first.
pub fn info() -> Value {
    let locale = glib::language_names()
        .first()
        .map(|name| name.to_string())
        .unwrap_or_else(|| "en".into());
    json!({
        "platform": "linux",
        "version": crate::VERSION,
        "locale": locale,
        "titleBar": { "insetLeft": 0, "windowButtons": true },
    })
}

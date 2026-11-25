//! What every page needs: the engine, the window (for dialogs and toasts),
//! the saved settings and the latest state.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use linuxplay_proto::api;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::engine::Engine;
use crate::settings::Settings;

pub struct Ui {
    pub app: adw::Application,
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub engine: RefCell<Option<Engine>>,
    pub settings: RefCell<Settings>,
    pub state: RefCell<api::State>,
}

impl Ui {
    /// A short message at the bottom of the window.
    pub fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::builder().title(glib::markup_escape_text(message)).timeout(6).build());
    }

    pub async fn call<T: DeserializeOwned>(&self, name: &str, params: impl Serialize) -> Result<T, String> {
        let engine = self.engine.borrow().clone().ok_or("The LinuxPlay engine isn't running.")?;
        engine.call(name, params).await
    }

    /// Run `work` on the main loop; if it fails, say why in a toast.
    pub fn spawn(self: &Rc<Self>, work: impl Future<Output = Result<(), String>> + 'static) {
        let ui = self.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = work.await {
                ui.toast(&e);
            }
        });
    }

    /// Change the settings and save them.
    pub fn update_settings(&self, change: impl FnOnce(&mut Settings)) {
        let mut s = self.settings.borrow_mut();
        change(&mut s);
        s.save();
    }

    /// Ask before doing something that can't be undone; `then` runs on yes.
    pub fn confirm(self: &Rc<Self>, heading: &str, body: &str, action: &str, then: impl Fn() + 'static) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_responses(&[("cancel", "Cancel"), ("yes", action)]);
        dialog.set_response_appearance("yes", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(Some("yes"), move |_, _| then());
        dialog.present(Some(&self.window));
    }
}

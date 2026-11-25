//! Things the person at this computer must see at once, whatever page is
//! open: a PIN to read out to someone pairing, and a connection asking to be
//! let in. Each is a dialog that closes by itself when it no longer applies.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use linuxplay_proto::api::{self, method};

use crate::ui::Ui;

/// A connection's dialog, and whether it was already answered.
type Asking = (adw::AlertDialog, Rc<Cell<bool>>);

pub struct Prompts {
    ui: Rc<Ui>,
    pins: RefCell<HashMap<(String, String), adw::AlertDialog>>,
    waiting: RefCell<HashMap<u64, Asking>>,
}

impl Prompts {
    pub fn new(ui: &Rc<Ui>) -> Rc<Self> {
        Rc::new(Self { ui: ui.clone(), pins: RefCell::default(), waiting: RefCell::default() })
    }

    pub fn update(&self, state: &api::State) {
        let (pins, waiting) = match &state.hosting {
            Some(h) => (h.pins.as_slice(), h.waiting.as_slice()),
            None => (&[][..], &[][..]),
        };

        // PINs: show new ones, close the ones that are gone.
        let wanted: Vec<(String, String)> = pins.iter().map(|p| (p.who.clone(), p.pin.clone())).collect();
        self.pins.borrow_mut().retain(|key, dialog| {
            let keep = wanted.contains(key);
            if !keep {
                dialog.force_close();
            }
            keep
        });
        for (who, pin) in wanted {
            if self.pins.borrow().contains_key(&(who.clone(), pin.clone())) {
                continue;
            }
            let dialog = adw::AlertDialog::new(Some("Pairing"), Some(&format!("{who} is pairing with this computer. Type this PIN there:")));
            let label = gtk::Label::new(Some(&pin));
            label.add_css_class("title-1");
            label.add_css_class("numeric");
            dialog.set_extra_child(Some(&label));
            dialog.add_response("ok", "Done");
            dialog.set_close_response("ok");
            self.show(&dialog);
            self.pins.borrow_mut().insert((who, pin), dialog);
        }

        // Connections waiting for an answer.
        let ids: Vec<u64> = waiting.iter().map(|w| w.id).collect();
        self.waiting.borrow_mut().retain(|id, (dialog, answered)| {
            let keep = ids.contains(id);
            if !keep {
                answered.set(true);
                dialog.force_close();
            }
            keep
        });
        for w in waiting {
            if self.waiting.borrow().contains_key(&w.id) {
                continue;
            }
            let dialog = adw::AlertDialog::new(
                Some("Someone wants to connect"),
                Some(&format!("{} is asking to connect to this computer.", w.who)),
            );
            dialog.add_responses(&[("deny", "Deny"), ("view", "View only"), ("control", "Allow")]);
            dialog.set_response_appearance("deny", adw::ResponseAppearance::Destructive);
            dialog.set_response_appearance("control", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("control"));
            dialog.set_close_response("deny");
            let answered = Rc::new(Cell::new(false));
            let (ui, id, flag) = (self.ui.clone(), w.id, answered.clone());
            dialog.connect_response(None, move |_, response| {
                if flag.replace(true) {
                    return;
                }
                let decision = match response {
                    "control" => api::Decision::Control,
                    "view" => api::Decision::View,
                    _ => api::Decision::Deny,
                };
                decide(&ui, id, decision);
            });
            self.show(&dialog);
            self.waiting.borrow_mut().insert(w.id, (dialog, answered));
        }
    }

    /// Bring the window up (it may be hidden while sharing) and show `dialog`.
    fn show(&self, dialog: &adw::AlertDialog) {
        self.ui.window.present();
        dialog.present(Some(&self.ui.window));
    }
}

/// Answer a waiting connection.
pub fn decide(ui: &Rc<Ui>, id: u64, decision: api::Decision) {
    let ui2 = ui.clone();
    ui.spawn(async move { ui2.call::<serde_json::Value>(method::HOST_DECIDE, api::Decide { id, decision }).await.map(drop) });
}

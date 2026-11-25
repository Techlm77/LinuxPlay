//! Small pieces the pages share.

use std::cell::RefCell;
use std::time::{SystemTime, UNIX_EPOCH};

use adw::prelude::*;

/// A preferences group whose rows are replaced as a whole whenever what they
/// show changes; hidden while it has none (unless it has a placeholder).
pub struct RowList {
    pub group: adw::PreferencesGroup,
    rows: RefCell<Vec<gtk::Widget>>,
    placeholder: Option<adw::ActionRow>,
}

impl RowList {
    pub fn new(title: &str, description: Option<&str>, placeholder: Option<&str>) -> Self {
        let group = adw::PreferencesGroup::builder().title(title).build();
        if let Some(d) = description {
            group.set_description(Some(d));
        }
        let placeholder = placeholder.map(|text| {
            let row = adw::ActionRow::builder().title(text).build();
            row.add_css_class("dim-label");
            group.add(&row);
            row
        });
        group.set_visible(placeholder.is_some());
        Self { group, rows: RefCell::new(Vec::new()), placeholder }
    }

    pub fn set(&self, rows: Vec<gtk::Widget>) {
        for old in self.rows.borrow_mut().drain(..) {
            self.group.remove(&old);
        }
        for row in &rows {
            self.group.add(row);
        }
        if let Some(p) = &self.placeholder {
            p.set_visible(rows.is_empty());
        }
        self.group.set_visible(!rows.is_empty() || self.placeholder.is_some());
        *self.rows.borrow_mut() = rows;
    }
}

/// A button for the end of a row.
pub fn row_button(label: &str, style: Option<&str>) -> gtk::Button {
    let b = gtk::Button::builder().label(label).valign(gtk::Align::Center).build();
    if let Some(s) = style {
        b.add_css_class(s);
    }
    b
}

/// A flat icon button for the end of a row.
pub fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).valign(gtk::Align::Center).build();
    b.add_css_class("flat");
    b
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(unix: u64) -> String {
    let s = now().saturating_sub(unix);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// A fingerprint in groups of four, as the host's log prints it.
pub fn fingerprint(hex: &str) -> String {
    let upper = hex.to_uppercase();
    let groups: Vec<&str> = upper.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
    groups.join(" ")
}

/// Text for a time left in minutes: "25 min left", "2 h 5 min left".
pub fn minutes_left(m: u64) -> String {
    if m < 60 { format!("{m} min left") } else { format!("{} h {} min left", m / 60, m % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_fingerprints_read_naturally() {
        assert_eq!(ago(now()), "just now");
        assert_eq!(ago(now() - 300), "5 min ago");
        assert_eq!(ago(now() - 3 * 3600), "3 h ago");
        assert_eq!(ago(now() - 3 * 86_400), "3 days ago");
        assert_eq!(fingerprint("0d4e03171c68"), "0D4E 0317 1C68");
        assert_eq!(minutes_left(125), "2 h 5 min left");
    }
}

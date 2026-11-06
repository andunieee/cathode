//! A bar that narrows down a list of videos already at hand, by the words of their titles and
//! channels, by when they came out and by how long they are. Nothing is asked of YouTube or the
//! relays: it only sifts through what was listed.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::fetch::Entry;

/// How long the videos shown are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Length {
    #[default]
    Any,
    Short,
    Medium,
    Long,
    VeryLong,
}

impl Length {
    /// In the order of the menu that picks it.
    pub const ALL: [(Length, &str); 5] = [
        (Length::Any, "Any length"),
        (Length::Short, "Under 4 minutes"),
        (Length::Medium, "4 to 20 minutes"),
        (Length::Long, "20 to 60 minutes"),
        (Length::VeryLong, "Over an hour"),
    ];

    /// Videos of unknown length only fit when any length does.
    pub fn fits(self, duration: Option<f64>) -> bool {
        let Some(minutes) = duration.map(|seconds| seconds / 60.0) else {
            return self == Length::Any;
        };
        match self {
            Length::Any => true,
            Length::Short => minutes < 4.0,
            Length::Medium => (4.0..20.0).contains(&minutes),
            Length::Long => (20.0..60.0).contains(&minutes),
            Length::VeryLong => minutes >= 60.0,
        }
    }
}

/// How long ago the videos shown came out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Age {
    #[default]
    Any,
    Day,
    Week,
    Month,
    Year,
}

impl Age {
    /// In the order of the menu that picks it.
    pub const ALL: [(Age, &str); 5] = [
        (Age::Any, "Any date"),
        (Age::Day, "Today"),
        (Age::Week, "This week"),
        (Age::Month, "This month"),
        (Age::Year, "This year"),
    ];

    /// Videos of unknown date only fit when any date does.
    fn fits(self, published: Option<i64>, now: i64) -> bool {
        const DAY: i64 = 24 * 60 * 60;
        let days = match self {
            Age::Any => return true,
            Age::Day => 1,
            Age::Week => 7,
            Age::Month => 31,
            Age::Year => 366,
        };
        published.is_some_and(|published| now - published <= days * DAY)
    }
}

/// What the bar lets through.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Criteria {
    /// Words the title or the channel all have, in any case.
    pub words: String,
    pub age: Age,
    pub length: Length,
}

impl Criteria {
    /// Whether it lets everything through.
    pub fn is_empty(&self) -> bool {
        self.words.trim().is_empty() && self.age == Age::Any && self.length == Length::Any
    }

    fn matches_at(&self, entry: &Entry, now: i64) -> bool {
        let text = format!("{} {}", entry.title, entry.channel).to_lowercase();
        self.words
            .to_lowercase()
            .split_whitespace()
            .all(|word| text.contains(word))
            && self.age.fits(entry.published, now)
            && self.length.fits(entry.duration)
    }

    /// The entries it lets through, in their order.
    pub fn select(&self, entries: &[Entry]) -> Vec<Entry> {
        let now = glib::real_time() / 1_000_000;
        entries
            .iter()
            .filter(|entry| self.matches_at(entry, now))
            .cloned()
            .collect()
    }
}

/// The bar: a box of words, a menu of dates and one of lengths.
pub struct Filter {
    pub bar: gtk::Box,
    words: gtk::SearchEntry,
    age: gtk::DropDown,
    length: gtk::DropDown,
    clear_button: gtk::Button,
    /// While the bar is set from the code, when its changes aren't the user's.
    quiet: Cell<bool>,
}

impl Filter {
    /// `changed` is called whenever the user changes what the bar lets through.
    pub fn new(changed: impl Fn() + 'static) -> Rc<Self> {
        let words = gtk::SearchEntry::builder()
            .placeholder_text("Filter by words")
            .hexpand(true)
            .build();
        let menu = |names: Vec<&str>| {
            let dropdown = gtk::DropDown::from_strings(&names);
            dropdown.set_cursor(Some(&crate::ui::pointer()));
            dropdown
        };
        let age = menu(Age::ALL.iter().map(|(_, name)| *name).collect());
        let length = menu(Length::ALL.iter().map(|(_, name)| *name).collect());
        let clear_button = gtk::Button::builder()
            .cursor(&crate::ui::pointer())
            .icon_name("edit-clear-symbolic")
            .tooltip_text("Show all the videos")
            .css_classes(["flat"])
            .visible(false)
            .build();

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_start(12);
        bar.set_margin_end(12);
        bar.set_margin_bottom(6);
        bar.append(&words);
        bar.append(&age);
        bar.append(&length);
        bar.append(&clear_button);

        let filter = Rc::new(Self {
            bar,
            words,
            age,
            length,
            clear_button,
            quiet: Cell::new(false),
        });

        let changed = Rc::new(changed);
        let notify = {
            let filter = Rc::downgrade(&filter);
            move || {
                let Some(filter) = filter.upgrade() else {
                    return;
                };
                filter.clear_button.set_visible(!filter.criteria().is_empty());
                if !filter.quiet.get() {
                    changed();
                }
            }
        };
        let notify = Rc::new(notify);
        filter.words.connect_search_changed({
            let notify = notify.clone();
            move |_| notify()
        });
        for dropdown in [&filter.age, &filter.length] {
            let notify = notify.clone();
            dropdown.connect_selected_notify(move |_| notify());
        }
        filter.clear_button.connect_clicked({
            let filter = Rc::downgrade(&filter);
            let notify = notify.clone();
            move |_| {
                if let Some(filter) = filter.upgrade() {
                    filter.set_quietly(|| filter.reset());
                    notify();
                }
            }
        });
        filter
    }

    pub fn criteria(&self) -> Criteria {
        Criteria {
            words: self.words.text().to_string(),
            age: Age::ALL
                .get(self.age.selected() as usize)
                .map(|(age, _)| *age)
                .unwrap_or_default(),
            length: Length::ALL
                .get(self.length.selected() as usize)
                .map(|(length, _)| *length)
                .unwrap_or_default(),
        }
    }

    /// Lets everything through again, without telling.
    pub fn clear(&self) {
        self.set_quietly(|| self.reset());
        self.clear_button.set_visible(false);
    }

    fn reset(&self) {
        self.words.set_text("");
        self.age.set_selected(0);
        self.length.set_selected(0);
    }

    fn set_quietly(&self, change: impl FnOnce()) {
        self.quiet.set(true);
        change();
        self.quiet.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, channel: &str, minutes: Option<f64>, published: Option<i64>) -> Entry {
        Entry {
            id: String::new(),
            url: String::new(),
            title: title.to_string(),
            channel: channel.to_string(),
            channel_id: String::new(),
            duration: minutes.map(|minutes| minutes * 60.0),
            views: None,
            published,
        }
    }

    #[test]
    fn criteria_match_words_dates_and_lengths() {
        const DAY: i64 = 24 * 60 * 60;
        let now = 1000 * DAY;
        let talk = entry("Rust in Production", "Conf Talks", Some(45.0), Some(now - 3 * DAY));
        let clip = entry("A short clip", "Someone", Some(2.0), None);

        let words = |words: &str| Criteria {
            words: words.to_string(),
            ..Criteria::default()
        };
        assert!(Criteria::default().is_empty());
        assert!(words("rust TALKS").matches_at(&talk, now));
        assert!(!words("rust clip").matches_at(&talk, now));
        assert!(words("  ").is_empty());

        let age = |age| Criteria {
            age,
            ..Criteria::default()
        };
        assert!(age(Age::Week).matches_at(&talk, now));
        assert!(!age(Age::Day).matches_at(&talk, now));
        // Without a date, only any date fits.
        assert!(!age(Age::Year).matches_at(&clip, now));
        assert!(age(Age::Any).matches_at(&clip, now));

        let length = |length| Criteria {
            length,
            ..Criteria::default()
        };
        assert!(length(Length::Long).matches_at(&talk, now));
        assert!(length(Length::Short).matches_at(&clip, now));
        assert!(!length(Length::Short).matches_at(&talk, now));
    }
}

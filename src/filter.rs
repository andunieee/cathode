//! A bar that narrows down a list of videos already at hand, by the words of their titles and
//! channels, by their hashtags, by when they came out and by how long they are, and puts them in
//! another order. Nothing is asked of YouTube or the relays: it only sifts through what was
//! listed.

use std::cell::Cell;
use std::cmp::Ordering;
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

/// The order the videos are shown in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    /// The order they were listed in.
    #[default]
    Listed,
    Newest,
    Oldest,
    Title,
    Shortest,
    Longest,
}

impl Sort {
    /// In the order of the menu that picks it. The name of the order they were listed in depends
    /// on the list.
    pub const ALL: [(Sort, &str); 6] = [
        (Sort::Listed, "Listed order"),
        (Sort::Newest, "Newest first"),
        (Sort::Oldest, "Oldest first"),
        (Sort::Title, "By title"),
        (Sort::Shortest, "Shortest first"),
        (Sort::Longest, "Longest first"),
    ];

    /// Which of the two goes first. The videos of unknown date or length go after the others.
    pub fn compare(self, a: &Entry, b: &Entry) -> Ordering {
        fn known_first<T>(
            a: Option<T>,
            b: Option<T>,
            order: impl Fn(T, T) -> Ordering,
        ) -> Ordering {
            match (a, b) {
                (Some(a), Some(b)) => order(a, b),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        }
        match self {
            Sort::Listed => Ordering::Equal,
            Sort::Newest => known_first(a.published, b.published, |a, b| b.cmp(&a)),
            Sort::Oldest => known_first(a.published, b.published, |a, b| a.cmp(&b)),
            Sort::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            Sort::Shortest => known_first(a.duration, b.duration, |a, b| a.total_cmp(&b)),
            Sort::Longest => known_first(a.duration, b.duration, |a, b| b.total_cmp(&a)),
        }
    }
}

/// What the bar lets through, and in what order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Criteria {
    /// Words the title or the channel all have, in any case, and hashtags, words with a `#`
    /// before them, the video all has.
    pub words: String,
    pub age: Age,
    pub length: Length,
    pub sort: Sort,
}

impl Criteria {
    /// Whether it lets everything through, in whatever order.
    pub fn is_empty(&self) -> bool {
        self.words.trim().is_empty() && self.age == Age::Any && self.length == Length::Any
    }

    /// The words and the hashtags, in lowercase and without their `#`.
    fn terms(&self) -> (Vec<String>, Vec<String>) {
        let mut words = Vec::new();
        let mut hashtags = Vec::new();
        for term in self.words.to_lowercase().split_whitespace() {
            match term.strip_prefix('#') {
                Some("") => {}
                Some(hashtag) => hashtags.push(hashtag.to_string()),
                None => words.push(term.to_string()),
            }
        }
        (words, hashtags)
    }

    /// `hashtags_of` tells the hashtags of a video, in lowercase and without their `#`. It is only
    /// asked when some are looked for.
    fn matches_at(
        &self,
        entry: &Entry,
        now: i64,
        (words, hashtags): &(Vec<String>, Vec<String>),
        hashtags_of: &impl Fn(&Entry) -> Vec<String>,
    ) -> bool {
        let text = format!("{} {}", entry.title, entry.channel).to_lowercase();
        words.iter().all(|word| text.contains(word.as_str()))
            && self.age.fits(entry.published, now)
            && self.length.fits(entry.duration)
            && (hashtags.is_empty() || {
                let has = hashtags_of(entry);
                hashtags.iter().all(|hashtag| has.contains(hashtag))
            })
    }

    /// The entries it lets through, in the order it asks for. `hashtags_of` tells the hashtags of
    /// a video, in lowercase and without their `#`.
    pub fn select(
        &self,
        entries: &[Entry],
        hashtags_of: impl Fn(&Entry) -> Vec<String>,
    ) -> Vec<Entry> {
        let now = glib::real_time() / 1_000_000;
        let terms = self.terms();
        let mut chosen: Vec<Entry> = entries
            .iter()
            .filter(|entry| self.matches_at(entry, now, &terms, &hashtags_of))
            .cloned()
            .collect();
        // The sort is stable, the ones it can't tell apart stay in the order they were listed in.
        chosen.sort_by(|a, b| self.sort.compare(a, b));
        chosen
    }
}

/// The bar: a box of words, a menu of dates, one of lengths and one of orders, after any menus
/// of the list's own.
pub struct Filter {
    pub bar: gtk::Box,
    words: gtk::SearchEntry,
    age: gtk::DropDown,
    length: gtk::DropDown,
    sort: gtk::DropDown,
    /// The menus of the list's own, which show everything on their first item.
    menus: Vec<gtk::DropDown>,
    clear_button: gtk::Button,
    /// While the bar is set from the code, when its changes aren't the user's.
    quiet: Cell<bool>,
}

impl Filter {
    /// `listed` names the order the videos were listed in, `menus` are put before the bar's own
    /// and cleared with them. `changed` is called whenever the user changes what the bar lets
    /// through, or in what order, or a menu of `menus` changes.
    pub fn new(listed: &str, menus: Vec<gtk::DropDown>, changed: impl Fn() + 'static) -> Rc<Self> {
        let words = gtk::SearchEntry::builder()
            .placeholder_text("Filter by words or #hashtags")
            .hexpand(true)
            .build();
        let menu = |names: Vec<&str>| {
            let dropdown = gtk::DropDown::from_strings(&names);
            dropdown.set_cursor(Some(&crate::ui::pointer()));
            dropdown
        };
        let age = menu(Age::ALL.iter().map(|(_, name)| *name).collect());
        let length = menu(Length::ALL.iter().map(|(_, name)| *name).collect());
        let sort = menu(
            Sort::ALL
                .iter()
                .map(|(sort, name)| if *sort == Sort::Listed { listed } else { *name })
                .collect(),
        );
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
        for menu in &menus {
            bar.append(menu);
        }
        bar.append(&words);
        bar.append(&age);
        bar.append(&length);
        bar.append(&sort);
        bar.append(&clear_button);

        let filter = Rc::new(Self {
            bar,
            words,
            age,
            length,
            sort,
            menus,
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
                filter.clear_button.set_visible(!filter.is_clear());
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
        for dropdown in [&filter.age, &filter.length, &filter.sort]
            .into_iter()
            .chain(&filter.menus)
        {
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
            sort: Sort::ALL
                .get(self.sort.selected() as usize)
                .map(|(sort, _)| *sort)
                .unwrap_or_default(),
        }
    }

    /// Whether everything is shown in the order it was listed in.
    pub fn is_clear(&self) -> bool {
        self.criteria() == Criteria::default() && self.menus.iter().all(|menu| menu.selected() == 0)
    }

    /// Shows the clear button when there is something to clear, after the code changed the
    /// menus of the list's own without telling.
    pub fn refresh(&self) {
        self.clear_button.set_visible(!self.is_clear());
    }

    /// Narrows the videos down to the ones with the hashtag too, as typing it would.
    pub fn add_hashtag(&self, hashtag: &str) {
        let hashtag = format!("#{}", hashtag.trim_start_matches('#').to_lowercase());
        let text = self.words.text();
        if text
            .split_whitespace()
            .any(|word| word.to_lowercase() == hashtag)
        {
            return;
        }
        let text = match text.trim_end() {
            "" => hashtag,
            words => format!("{words} {hashtag}"),
        };
        self.words.set_text(&text);
        self.words.set_position(-1);
    }

    /// Lets everything through again, in the order it was listed in, without telling.
    pub fn clear(&self) {
        self.set_quietly(|| self.reset());
        self.clear_button.set_visible(false);
    }

    fn reset(&self) {
        self.words.set_text("");
        for dropdown in [&self.age, &self.length, &self.sort]
            .into_iter()
            .chain(&self.menus)
        {
            dropdown.set_selected(0);
        }
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

    /// The hashtags of the videos in the tests: those of the words of their channel.
    fn hashtags_of(entry: &Entry) -> Vec<String> {
        entry
            .channel
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    fn fits(criteria: &Criteria, entry: &Entry, now: i64) -> bool {
        criteria.matches_at(entry, now, &criteria.terms(), &hashtags_of)
    }

    #[test]
    fn criteria_match_words_hashtags_dates_and_lengths() {
        const DAY: i64 = 24 * 60 * 60;
        let now = 1000 * DAY;
        let talk = entry(
            "Rust in Production",
            "Conf Talks",
            Some(45.0),
            Some(now - 3 * DAY),
        );
        let clip = entry("A short clip", "Someone", Some(2.0), None);

        let words = |words: &str| Criteria {
            words: words.to_string(),
            ..Criteria::default()
        };
        assert!(Criteria::default().is_empty());
        assert!(fits(&words("rust TALKS"), &talk, now));
        assert!(!fits(&words("rust clip"), &talk, now));
        assert!(words("  ").is_empty());

        // Every hashtag has to be the video's, not just in its title.
        assert!(fits(&words("#conf #TALKS"), &talk, now));
        assert!(fits(&words("rust #talks"), &talk, now));
        assert!(!fits(&words("#talks #someone"), &talk, now));
        assert!(!fits(&words("#rust"), &talk, now));
        assert!(fits(&words("# clip"), &clip, now));

        let age = |age| Criteria {
            age,
            ..Criteria::default()
        };
        assert!(fits(&age(Age::Week), &talk, now));
        assert!(!fits(&age(Age::Day), &talk, now));
        // Without a date, only any date fits.
        assert!(!fits(&age(Age::Year), &clip, now));
        assert!(fits(&age(Age::Any), &clip, now));

        let length = |length| Criteria {
            length,
            ..Criteria::default()
        };
        assert!(fits(&length(Length::Long), &talk, now));
        assert!(fits(&length(Length::Short), &clip, now));
        assert!(!fits(&length(Length::Short), &talk, now));
    }

    #[test]
    fn sorts_put_the_unknown_last() {
        let entries = [
            entry("beta", "", Some(10.0), Some(200)),
            entry("Alpha", "", None, Some(300)),
            entry("gamma", "", Some(3.0), None),
            entry("delta", "", Some(60.0), Some(100)),
        ];
        let titles = |sort| -> Vec<String> {
            Criteria {
                sort,
                ..Criteria::default()
            }
            .select(&entries, hashtags_of)
            .into_iter()
            .map(|entry| entry.title)
            .collect()
        };
        assert_eq!(titles(Sort::Listed), ["beta", "Alpha", "gamma", "delta"]);
        assert_eq!(titles(Sort::Newest), ["Alpha", "beta", "delta", "gamma"]);
        assert_eq!(titles(Sort::Oldest), ["delta", "beta", "Alpha", "gamma"]);
        assert_eq!(titles(Sort::Title), ["Alpha", "beta", "delta", "gamma"]);
        assert_eq!(titles(Sort::Shortest), ["gamma", "beta", "delta", "Alpha"]);
        assert_eq!(titles(Sort::Longest), ["delta", "beta", "gamma", "Alpha"]);
    }
}

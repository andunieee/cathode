pub fn duration(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    let (hours, minutes, seconds) = (secs / 3600, (secs / 60) % 60, secs % 60);

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

pub fn count(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=999_999 => format!("{:.1}K", count as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", count as f64 / 1e6),
        _ => format!("{:.1}B", count as f64 / 1e9),
    }
}

pub fn bytes(bytes: u64) -> String {
    match bytes {
        0..=999_999 => format!("{:.0} KB", bytes as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1} MB", bytes as f64 / 1e6),
        _ => format!("{:.2} GB", bytes as f64 / 1e9),
    }
}

/// yt-dlp reports dates as YYYYMMDD.
pub fn date(date: &str) -> String {
    if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..])
    } else {
        date.to_string()
    }
}

/// How long ago `published` was, as "3 days ago".
pub fn ago(published: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |now| now.as_secs() as i64);
    let secs = (now - published).max(0);

    let (count, unit) = match secs {
        0..=3_599 => (secs / 60, "minute"),
        3_600..=86_399 => (secs / 3_600, "hour"),
        86_400..=604_799 => (secs / 86_400, "day"),
        604_800..=2_591_999 => (secs / 604_800, "week"),
        2_592_000..=31_535_999 => (secs / 2_592_000, "month"),
        _ => (secs / 31_536_000, "year"),
    };

    match count {
        0 => "just now".to_string(),
        1 => format!("1 {unit} ago"),
        count => format!("{count} {unit}s ago"),
    }
}

/// Seconds since the epoch as YYYY-MM-DD, in UTC.
pub fn epoch_date(secs: u64) -> String {
    // Civil-from-days, https://howardhinnant.github.io/date_algorithms.html
    let days = (secs / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    format!("{year}-{month:02}-{day:02}")
}

/// The name of a language from its tag, like `pt-BR` or the `pt-orig` of YouTube's captions of
/// the speech as it was said. Tags that aren't known are shown as they are.
pub fn language_name(tag: &str) -> String {
    const NAMES: [(&str, &str); 38] = [
        ("ar", "Arabic"),
        ("cs", "Czech"),
        ("da", "Danish"),
        ("de", "German"),
        ("el", "Greek"),
        ("en", "English"),
        ("es", "Spanish"),
        ("es-419", "Spanish (Latin America)"),
        ("fa", "Persian"),
        ("fi", "Finnish"),
        ("fr", "French"),
        ("he", "Hebrew"),
        ("hi", "Hindi"),
        ("hu", "Hungarian"),
        ("id", "Indonesian"),
        ("it", "Italian"),
        ("iw", "Hebrew"),
        ("ja", "Japanese"),
        ("ko", "Korean"),
        ("nb", "Norwegian"),
        ("nl", "Dutch"),
        ("no", "Norwegian"),
        ("pl", "Polish"),
        ("pt", "Portuguese"),
        ("pt-BR", "Portuguese (Brazil)"),
        ("pt-PT", "Portuguese (Portugal)"),
        ("ro", "Romanian"),
        ("ru", "Russian"),
        ("sv", "Swedish"),
        ("th", "Thai"),
        ("tr", "Turkish"),
        ("uk", "Ukrainian"),
        ("vi", "Vietnamese"),
        ("zh", "Chinese"),
        ("zh-CN", "Chinese (China)"),
        ("zh-Hans", "Chinese (Simplified)"),
        ("zh-Hant", "Chinese (Traditional)"),
        ("zh-TW", "Chinese (Taiwan)"),
    ];

    let (tag, original) = match tag.strip_suffix("-orig") {
        Some(tag) => (tag, true),
        None => (tag, false),
    };
    let find = |wanted: &str| {
        NAMES
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(wanted))
            .map(|(_, name)| name.to_string())
    };
    let name = find(tag)
        .or_else(|| tag.split(['-', '_']).next().and_then(find))
        .unwrap_or_else(|| tag.to_string());
    if original {
        format!("{name} (original)")
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_have_names() {
        assert_eq!(language_name("en"), "English");
        assert_eq!(language_name("pt-BR"), "Portuguese (Brazil)");
        assert_eq!(language_name("pt-orig"), "Portuguese (original)");
        assert_eq!(language_name("en-US"), "English");
        assert_eq!(language_name("xx"), "xx");
    }
}

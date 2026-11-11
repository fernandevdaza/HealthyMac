//! Human-readable formatting in the current language (see [`crate::i18n`]). Each function has an
//! `_in` twin that takes the language, so tests never have to flip the process-wide setting.

use std::time::SystemTime;

use crate::i18n::{language, Language};

/// "1,5" in Spanish, "1.5" in English.
fn decimal(text: String, lang: Language) -> String {
    match lang {
        Language::Spanish => text.replace('.', ","),
        Language::English => text,
    }
}

/// Formats bytes in decimal units, matching Finder ("1,2 GB").
pub fn human_size(bytes: u64) -> String {
    human_size_in(bytes, language())
}

pub fn human_size_in(bytes: u64, lang: Language) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    let text = if value >= 100.0 {
        format!("{value:.0}")
    } else {
        decimal(format!("{value:.1}"), lang)
    };
    format!("{text} {}", UNITS[unit])
}

/// Formats memory in binary units, as About This Mac and Activity Monitor do, so that 48 GiB of RAM
/// reads "48 GB" rather than the "51,5 GB" that [`human_size`] would give.
pub fn human_memory(bytes: u64) -> String {
    human_memory_in(bytes, language())
}

pub fn human_memory_in(bytes: u64, lang: Language) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        let gib = value / GIB;
        if (gib - gib.round()).abs() < 0.05 {
            format!("{gib:.0} GB")
        } else {
            format!("{} GB", decimal(format!("{gib:.1}"), lang))
        }
    } else {
        format!("{:.0} MB", value / MIB)
    }
}

/// Formats how long ago `then` was, relative to `now` ("hace 3 días", "3 days ago").
pub fn human_age(then: SystemTime, now: SystemTime) -> String {
    human_age_in(then, now, language())
}

pub fn human_age_in(then: SystemTime, now: SystemTime, lang: Language) -> String {
    let secs = now.duration_since(then).map(|d| d.as_secs()).unwrap_or(0);
    let english = lang == Language::English;
    let plural = |n: u64, units: [&str; 4]| {
        let [one_es, many_es, one_en, many_en] = units;
        match (english, n) {
            (false, 1) => format!("hace 1 {one_es}"),
            (false, _) => format!("hace {n} {many_es}"),
            (true, 1) => format!("1 {one_en} ago"),
            (true, _) => format!("{n} {many_en} ago"),
        }
    };
    match secs {
        0..=59 if english => "just now".to_string(),
        0..=59 => "ahora mismo".to_string(),
        60..=3_599 => plural(secs / 60, ["minuto", "minutos", "minute", "minutes"]),
        3_600..=86_399 => plural(secs / 3_600, ["hora", "horas", "hour", "hours"]),
        86_400..=2_591_999 => plural(secs / 86_400, ["día", "días", "day", "days"]),
        2_592_000..=31_535_999 => plural(secs / 2_592_000, ["mes", "meses", "month", "months"]),
        _ => plural(secs / 31_536_000, ["año", "años", "year", "years"]),
    }
}

/// Whole days between `then` and `now`, or `None` if `then` is in the future.
pub fn age_days(then: SystemTime, now: SystemTime) -> Option<u64> {
    now.duration_since(then).ok().map(|d| d.as_secs() / 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_000), "1,0 KB");
        assert_eq!(human_size(1_500_000), "1,5 MB");
        assert_eq!(human_size(245_000_000_000), "245 GB");
    }

    #[test]
    fn memory_uses_binary_units() {
        assert_eq!(human_memory(48 * 1024 * 1024 * 1024), "48 GB");
        assert_eq!(human_memory(40_200_896_512), "37,4 GB");
        assert_eq!(human_memory(512 * 1024 * 1024), "512 MB");
    }

    #[test]
    fn ages() {
        let now = SystemTime::now();
        let ago = |secs| now - Duration::from_secs(secs);
        assert_eq!(human_age(ago(5), now), "ahora mismo");
        assert_eq!(human_age(ago(60), now), "hace 1 minuto");
        assert_eq!(human_age(ago(86_400 * 3), now), "hace 3 días");
        assert_eq!(human_age(ago(31_536_000 * 2), now), "hace 2 años");
        assert_eq!(human_age(now + Duration::from_secs(50), now), "ahora mismo");
    }

    #[test]
    fn english() {
        let en = Language::English;
        assert_eq!(human_size_in(1_500_000, en), "1.5 MB");
        assert_eq!(human_memory_in(40_200_896_512, en), "37.4 GB");
        let now = SystemTime::now();
        let ago = |secs: u64| now - Duration::from_secs(secs);
        assert_eq!(human_age_in(ago(5), now, en), "just now");
        assert_eq!(human_age_in(ago(60), now, en), "1 minute ago");
        assert_eq!(human_age_in(ago(86_400 * 3), now, en), "3 days ago");
    }
}

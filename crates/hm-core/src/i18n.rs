//! Spanish and English. Spanish is the source language: every user-facing string is written in
//! Spanish first, right next to its English, with [`t!`](crate::t). Scanners run on worker threads,
//! so the current language is a process-wide atomic rather than something passed around.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Spanish,
    English,
}

/// Switches every string produced from now on. What was already produced, such as the findings of
/// an earlier scan, keeps the language it was made in.
pub fn set_language(language: Language) {
    ENGLISH.store(language == Language::English, Ordering::Relaxed);
}

pub fn language() -> Language {
    if is_english() {
        Language::English
    } else {
        Language::Spanish
    }
}

pub fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// Picks the Spanish or the English text for the current language, as a `&'static str`.
///
/// For text with values in it, use [`tf!`](crate::tf). A brace in a `t!` literal is shown as is.
///
/// ```
/// use hm_core::t;
/// let title: &str = t!("Papelera", "Trash");
/// ```
#[macro_export]
macro_rules! t {
    ($es:literal, $en:literal $(,)?) => {
        if $crate::i18n::is_english() {
            $en
        } else {
            $es
        }
    };
}

/// Like [`t!`](crate::t), but formats: both templates take the same arguments, inline (`{count}`)
/// or after them, and `format!` checks both at compile time. Yields a `String`.
///
/// ```
/// use hm_core::tf;
/// let count = 3;
/// let line: String = tf!("{count} elementos", "{count} items");
/// let line: String = tf!("{} de {}", "{} of {}", 1, 2);
/// ```
#[macro_export]
macro_rules! tf {
    ($es:literal, $en:literal $(,)?) => {
        if $crate::i18n::is_english() {
            format!($en)
        } else {
            format!($es)
        }
    };
    ($es:literal, $en:literal, $($arg:tt)+) => {
        if $crate::i18n::is_english() {
            format!($en, $($arg)+)
        } else {
            format!($es, $($arg)+)
        }
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn spanish_is_the_default() {
        assert_eq!(crate::t!("Papelera", "Trash"), "Papelera");
        assert_eq!(crate::tf!("{} de {}", "{} of {}", 1, 2), "1 de 2");
        let count = 3;
        assert_eq!(
            crate::tf!("{count} elementos", "{count} items"),
            "3 elementos"
        );
    }
}

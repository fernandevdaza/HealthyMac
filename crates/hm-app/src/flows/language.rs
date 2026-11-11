//! Spanish or English. Follows the first language macOS prefers unless the user picked one.

use gpui::{Context, Window};
use hm_core::i18n::{set_language, Language};
use hm_core::LanguageChoice;

use crate::state::Shell;

/// What macOS lists first in Language & Region. Spanish of any region counts as Spanish, anything
/// else as English. If the list cannot be read, Spanish, which is what the app spoke before.
pub fn system_language() -> Language {
    let output = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output();
    let Ok(output) = output else {
        return Language::Spanish;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    // The output is a plist array: ( "es-BO", "en-US" ).
    match text.split('"').nth(1) {
        Some(first) if first.starts_with("es") => Language::Spanish,
        Some(_) => Language::English,
        None => Language::Spanish,
    }
}

pub fn resolve(choice: LanguageChoice) -> Language {
    match choice {
        LanguageChoice::System => system_language(),
        LanguageChoice::Spanish => Language::Spanish,
        LanguageChoice::English => Language::English,
    }
}

impl Shell {
    /// Switches the interface now. Findings already on screen keep their language until the next
    /// scan, because their text was written when they were found.
    pub fn set_language_choice(
        &mut self,
        choice: LanguageChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_config(cx, |config| config.language = choice);
        set_language(resolve(choice));
        self.scanners = crate::flows::scan::scanners_for(&self.mode);
        cx.set_menus(crate::menus());
        window.refresh();
    }
}

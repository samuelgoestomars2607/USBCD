//! Zweisprachigkeit: Deutsch, wenn Windows auf Deutsch eingestellt ist, sonst Englisch.

use std::sync::OnceLock;

static GERMAN: OnceLock<bool> = OnceLock::new();

pub fn is_german() -> bool {
    *GERMAN.get_or_init(ui_is_german)
}

#[cfg(windows)]
fn ui_is_german() -> bool {
    // primäre Sprach-ID 0x07 = Deutsch (de-DE, de-AT, de-CH …)
    let lang = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    lang & 0x3ff == 0x07
}

#[cfg(not(windows))]
fn ui_is_german() -> bool {
    std::env::var("LANG")
        .map(|l| l.starts_with("de"))
        .unwrap_or(false)
}

/// Text in der Sprache von Windows.
pub fn tr<'a>(de: &'a str, en: &'a str) -> &'a str {
    if is_german() {
        de
    } else {
        en
    }
}

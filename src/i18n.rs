//! Langue de l'interface : français ou anglais (américain), choisie dans les options ou d'après
//! celle du système : français ou anglais selon le système, anglais pour toute autre langue.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::Language;

/// Français tant qu'aucune langue n'a été choisie (les tests sont écrits en français).
static FRENCH: AtomicBool = AtomicBool::new(true);

/// Texte dans la langue choisie : `tr!("français", "English")`, avec des arguments `{x}` comme
/// pour `format!`.
macro_rules! tr {
    ($fr:literal, $en:literal $(,)?) => {
        if $crate::i18n::french() {
            format!($fr)
        } else {
            format!($en)
        }
    };
}

pub fn set(language: Language) {
    FRENCH.store(
        is_french(language, sys_locale::get_locale().as_deref()),
        Ordering::Relaxed,
    );
}

pub fn french() -> bool {
    FRENCH.load(Ordering::Relaxed)
}

/// Langue retenue, `locale` étant celle du système (`fr-FR`, `en-US`…).
fn is_french(language: Language, locale: Option<&str>) -> bool {
    match language {
        Language::French => true,
        Language::English => false,
        Language::Auto => locale.is_some_and(|l| l.to_ascii_lowercase().starts_with("fr")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        assert!(is_french(Language::Auto, Some("fr-FR")));
        assert!(is_french(Language::Auto, Some("fr_CA")));
        assert!(!is_french(Language::Auto, Some("en-US")));
        assert!(!is_french(Language::Auto, Some("de-DE")));
        assert!(!is_french(Language::Auto, None));
        assert!(is_french(Language::French, Some("en-US")));
        assert!(!is_french(Language::English, Some("fr-FR")));
    }
}

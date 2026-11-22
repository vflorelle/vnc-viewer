//! Codes de touches X11 (« keysyms ») envoyés au serveur.

use eframe::egui::Key;

pub const BACKSPACE: u32 = 0xff08;
pub const TAB: u32 = 0xff09;
pub const RETURN: u32 = 0xff0d;
pub const ESCAPE: u32 = 0xff1b;
pub const HOME: u32 = 0xff50;
pub const LEFT: u32 = 0xff51;
pub const UP: u32 = 0xff52;
pub const RIGHT: u32 = 0xff53;
pub const DOWN: u32 = 0xff54;
pub const PAGE_UP: u32 = 0xff55;
pub const PAGE_DOWN: u32 = 0xff56;
pub const END: u32 = 0xff57;
pub const INSERT: u32 = 0xff63;
pub const DELETE: u32 = 0xffff;
pub const F1: u32 = 0xffbe;
pub const SHIFT_L: u32 = 0xffe1;
pub const CONTROL_L: u32 = 0xffe3;
pub const ALT_L: u32 = 0xffe9;
pub const SUPER_L: u32 = 0xffeb;
pub const SUPER_R: u32 = 0xffec;
pub const PRINT: u32 = 0xff61;
pub const PAUSE: u32 = 0xff13;
pub const MENU: u32 = 0xff67;
pub const VOLUME_MUTE: u32 = 0x1008_ff12;
pub const VOLUME_DOWN: u32 = 0x1008_ff11;
pub const VOLUME_UP: u32 = 0x1008_ff13;
pub const MEDIA_PLAY: u32 = 0x1008_ff14;
pub const MEDIA_STOP: u32 = 0x1008_ff15;
pub const MEDIA_PREVIOUS: u32 = 0x1008_ff16;
pub const MEDIA_NEXT: u32 = 0x1008_ff17;

/// Touches envoyées au serveur seulement si le réglage correspondant le permet.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Group {
    /// Touche Windows, Impr. écran (et Alt+Tab, Alt+Échap, Ctrl+Échap, vus par le crochet).
    Special,
    Volume,
    Media,
}

pub fn group(keysym: u32) -> Option<Group> {
    match keysym {
        SUPER_L | SUPER_R | PRINT => Some(Group::Special),
        VOLUME_MUTE | VOLUME_DOWN | VOLUME_UP => Some(Group::Volume),
        MEDIA_PLAY | MEDIA_STOP | MEDIA_PREVIOUS | MEDIA_NEXT => Some(Group::Media),
        _ => None,
    }
}

/// Caractère tapé : Latin-1 tel quel, le reste en Unicode (0x01000000 + point de code).
pub fn char_keysym(c: char) -> u32 {
    match u32::from(c) {
        u @ (0x20..=0x7e | 0xa0..=0xff) => u,
        u => 0x0100_0000 | u,
    }
}

/// Touche sans caractère (flèches, Entrée, F1…, Impr. écran, Windows, Menu, volume, multimédia).
pub fn special(key: Key) -> Option<u32> {
    Some(match key {
        Key::ArrowDown => DOWN,
        Key::ArrowLeft => LEFT,
        Key::ArrowRight => RIGHT,
        Key::ArrowUp => UP,
        Key::Escape => ESCAPE,
        Key::Tab => TAB,
        Key::Backspace => BACKSPACE,
        Key::Enter => RETURN,
        Key::Insert => INSERT,
        Key::Delete => DELETE,
        Key::Home => HOME,
        Key::End => END,
        Key::PageUp => PAGE_UP,
        Key::PageDown => PAGE_DOWN,
        Key::PrintScreen => PRINT,
        Key::Pause => PAUSE,
        Key::Menu => MENU,
        Key::SuperLeft => SUPER_L,
        Key::SuperRight => SUPER_R,
        Key::AudioVolumeMute => VOLUME_MUTE,
        Key::AudioVolumeDown => VOLUME_DOWN,
        Key::AudioVolumeUp => VOLUME_UP,
        Key::MediaPlayPause => MEDIA_PLAY,
        Key::MediaStop => MEDIA_STOP,
        Key::MediaTrackPrevious => MEDIA_PREVIOUS,
        Key::MediaTrackNext => MEDIA_NEXT,
        _ => {
            let name = key.name();
            let n: u32 = name.strip_prefix('F')?.parse().ok()?;
            return (1..=35).contains(&n).then(|| F1 + n - 1);
        }
    })
}

/// Touche de caractère, telle qu'envoyée avec Ctrl ou Alt : le caractère qu'elle produit, Maj
/// comprise, comme le veut le protocole (Ctrl+A : « a », Ctrl+Maj+A : « A »). La touche donnée par
/// la fenêtre tient déjà compte de Maj pour la ponctuation (« ! », « ? »…), pas pour la casse des
/// lettres. Un serveur qui recevrait « a » avec Maj appuyée relâcherait Maj pour produire « a ».
pub fn printable(key: Key, shift: bool) -> Option<u32> {
    let c = match key {
        Key::Space => ' ',
        Key::Colon => ':',
        Key::Comma => ',',
        Key::Backslash => '\\',
        Key::Slash => '/',
        Key::Pipe => '|',
        Key::Questionmark => '?',
        Key::Exclamationmark => '!',
        Key::OpenBracket => '[',
        Key::CloseBracket => ']',
        Key::OpenCurlyBracket => '{',
        Key::CloseCurlyBracket => '}',
        Key::Backtick => '`',
        Key::Minus => '-',
        Key::Period => '.',
        Key::Plus => '+',
        Key::Equals => '=',
        Key::Semicolon => ';',
        Key::Quote => '\'',
        _ => {
            // Lettres et chiffres : leur nom est le caractère.
            let mut chars = key.name().chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() && shift => c.to_ascii_uppercase(),
                (Some(c), None) if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
                _ => return None,
            }
        }
    };
    Some(char_keysym(c))
}

/// Touches virtuelles de Windows sans caractère ; `extended` : touche du pavé de navigation
/// ou Entrée du pavé numérique.
#[cfg(windows)]
pub fn virtual_key(vk: u16, extended: bool) -> Option<u32> {
    Some(match vk {
        0x08 => BACKSPACE,
        0x09 => TAB,
        0x0d if extended => 0xff8d, // Entrée du pavé numérique
        0x0d => RETURN,
        0x13 => PAUSE,
        0x14 => 0xffe5, // Verr. Maj
        0x1b => ESCAPE,
        0x21 => PAGE_UP,
        0x22 => PAGE_DOWN,
        0x23 => END,
        0x24 => HOME,
        0x25 => LEFT,
        0x26 => UP,
        0x27 => RIGHT,
        0x28 => DOWN,
        0x2c => PRINT,
        0x2d => INSERT,
        0x2e => DELETE,
        0x5b => SUPER_L,
        0x5c => SUPER_R,
        0x5d => MENU,
        0x60..=0x69 => 0xffb0 + u32::from(vk - 0x60), // pavé numérique 0-9
        0x6a => 0xffaa,
        0x6b => 0xffab,
        0x6c => 0xffac,
        0x6d => 0xffad,
        0x6e => 0xffae,
        0x6f => 0xffaf,
        0x70..=0x87 => F1 + u32::from(vk - 0x70),
        0x90 => 0xff7f, // Verr. Num
        0x91 => 0xff14, // Arrêt défil.
        0xa0 => SHIFT_L,
        0xa1 => 0xffe2,
        0xa2 => CONTROL_L,
        0xa3 => 0xffe4,
        0xa4 => ALT_L,
        0xa5 => 0xffea,
        0xad => VOLUME_MUTE,
        0xae => VOLUME_DOWN,
        0xaf => VOLUME_UP,
        0xb0 => MEDIA_NEXT,
        0xb1 => MEDIA_PREVIOUS,
        0xb2 => MEDIA_STOP,
        0xb3 => MEDIA_PLAY,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms() {
        assert_eq!(char_keysym('a'), 0x61);
        assert_eq!(char_keysym('é'), 0xe9);
        assert_eq!(char_keysym('€'), 0x0100_20ac);
        assert_eq!(special(Key::F12), Some(F1 + 11));
        assert_eq!(special(Key::Enter), Some(RETURN));
        assert_eq!(special(Key::A), None);
        assert_eq!(special(Key::PrintScreen), Some(PRINT));
        assert_eq!(special(Key::Menu), Some(MENU));
        assert_eq!(group(PRINT), Some(Group::Special));
        assert_eq!(group(special(Key::AudioVolumeUp).unwrap()), Some(Group::Volume));
        assert_eq!(group(special(Key::MediaPlayPause).unwrap()), Some(Group::Media));
        assert_eq!(group(RETURN), None);
        assert_eq!(printable(Key::A, false), Some(0x61));
        assert_eq!(printable(Key::Z, true), Some(0x5a));
        assert_eq!(printable(Key::Num1, true), Some(0x31));
        assert_eq!(printable(Key::Minus, false), Some(0x2d));
        assert_eq!(printable(Key::Space, false), Some(0x20));
        assert_eq!(printable(Key::F1, false), None);
    }
}

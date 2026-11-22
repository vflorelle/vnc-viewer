//! Clavier sous Windows : les touches arrivent par la fenêtre (comme sous Linux), mais Windows en
//! garde certaines pour lui avant qu'elles n'y arrivent, et la fenêtre en ignore d'autres. Un
//! crochet bas niveau (WH_KEYBOARD_LL) les prend pour la session qui a le focus et les envoie au
//! serveur : Alt+F4, Menu et Pause toujours, et selon les réglages la touche Windows (avec toute
//! touche appuyée en même temps, comme Win+L), Impr. écran, Alt+Tab, Alt+Échap, Ctrl+Échap, le
//! volume, les touches multimédia. Il prend aussi Maj+Suppr, Ctrl+Inser et Maj+Inser, que la
//! fenêtre changerait en commandes du presse-papiers.
//!
//! Le crochet a son propre thread et sa boucle de messages : Windows retire sans prévenir un
//! crochet qui tarde à répondre, ce qui arriverait sur le thread de l'interface pendant qu'il
//! dessine.
//!
//! Windows n'appelle pas un tel crochet pour les touches destinées à une fenêtre du programme si
//! celui-ci est inscrit à l'entrée brute (« raw input ») du clavier, ce que fait la couche des
//! fenêtres : l'inscription est retirée à l'installation du crochet.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MAPVK_VK_TO_CHAR, MapVirtualKeyW, VIRTUAL_KEY, VK_DELETE, VK_ESCAPE, VK_F4, VK_INSERT, VK_LCONTROL, VK_LMENU,
    VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_TAB,
};
use windows::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_REMOVE, RegisterRawInputDevices};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG, SetWindowsHookExW,
    TranslateMessage, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use crate::config::Settings;
use crate::keys::{self, Group};
use crate::session::Keys;

/// Session qui reçoit le clavier (celle dont la fenêtre a le focus), et ce qu'elle prend à Windows.
#[derive(Clone)]
pub struct Target {
    /// Identifiant de la session (pour reconnaître un changement de session).
    pub id: u64,
    pub keys: Keys,
    pub settings: Settings,
}

struct Hook {
    target: Option<Target>,
    /// Touches appuyées envoyées au serveur, avec le keysym à relâcher.
    pressed: HashMap<u16, u32>,
    /// Ctrl, Alt et Maj appuyées (touches virtuelles gauche et droite).
    modifiers: HashSet<u16>,
}

static HOOK: LazyLock<Mutex<Hook>> = LazyLock::new(|| {
    Mutex::new(Hook {
        target: None,
        pressed: HashMap::new(),
        modifiers: HashSet::new(),
    })
});

static INSTALLED: AtomicBool = AtomicBool::new(false);

fn hook() -> MutexGuard<'static, Hook> {
    HOOK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Installe le crochet dans son thread (attend au plus une seconde qu'il soit en place).
pub fn install() {
    // La couche des fenêtres inscrit le programme à l'entrée brute du clavier, dont il n'a pas
    // besoin (les touches arrivent aussi par les messages des fenêtres) ; tant que cette
    // inscription existe, Windows n'appelle pas le crochet quand une fenêtre du programme a le
    // focus. Elle est retirée (la souris reste inscrite).
    let keyboard = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x06,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: HWND::default(),
    };
    // SAFETY : tableau d'une structure, de la taille annoncée.
    let _ = unsafe { RegisterRawInputDevices(&[keyboard], size_of::<RAWINPUTDEVICE>() as u32) };
    let (done, installed) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // SAFETY : procédure valable toute la durée du programme ; le crochet est retiré par
        // Windows à la fin du processus. La boucle de messages fait appeler le crochet.
        unsafe {
            let module = GetModuleHandleW(None).ok();
            let ok = SetWindowsHookExW(WH_KEYBOARD_LL, Some(procedure), module.map(Into::into), 0).is_ok();
            let _ = done.send(ok);
            if !ok {
                return;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });
    let ok = installed.recv_timeout(Duration::from_secs(1)).unwrap_or(false);
    INSTALLED.store(ok, Ordering::Relaxed);
}

/// Le crochet est en place.
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// Session qui reçoit le clavier (`None` : aucune) ; les touches restées appuyées pour la
/// précédente y sont relâchées. L'interface la tient à jour d'après le focus des fenêtres :
/// quand une session le perd (Alt+Tab laissé à Windows…), elle n'est plus désignée.
pub fn set_target(target: Option<Target>) {
    let mut hook = hook();
    if hook.target.as_ref().map(|t| t.id) != target.as_ref().map(|t| t.id) {
        let pressed = std::mem::take(&mut hook.pressed);
        if let Some(previous) = &hook.target {
            for keysym in pressed.into_values() {
                previous.keys.key(false, keysym);
            }
        }
        hook.modifiers.clear();
    }
    hook.target = target;
}

unsafe extern "system" fn procedure(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY : pour WH_KEYBOARD_LL, lparam pointe sur un KBDLLHOOKSTRUCT.
        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if hook().key(event, down) {
            return LRESULT(1);
        }
    }
    // SAFETY : transmission normale au crochet suivant.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

impl Hook {
    /// `true` : touche prise (envoyée au serveur, cachée à Windows et à la fenêtre).
    fn key(&mut self, event: &KBDLLHOOKSTRUCT, down: bool) -> bool {
        let Some(target) = self.target.clone() else {
            return false;
        };
        let vk = event.vkCode as u16;
        if [VK_LCONTROL, VK_RCONTROL, VK_LMENU, VK_RMENU, VK_LSHIFT, VK_RSHIFT]
            .iter()
            .any(|m| m.0 == vk)
        {
            // Touches de modification : suivies, et laissées à la fenêtre qui les envoie.
            if down {
                self.modifiers.insert(vk);
            } else {
                self.modifiers.remove(&vk);
            }
            return false;
        }
        let sender = &target.keys;
        if !down {
            return match self.pressed.remove(&vk) {
                Some(keysym) => {
                    sender.key(false, keysym);
                    true
                }
                None => false,
            };
        }
        let held = |left: VIRTUAL_KEY, right: VIRTUAL_KEY| {
            self.modifiers.contains(&left.0) || self.modifiers.contains(&right.0)
        };
        let ctrl = held(VK_LCONTROL, VK_RCONTROL);
        let alt = held(VK_LMENU, VK_RMENU);
        let shift = held(VK_LSHIFT, VK_RSHIFT);
        // Touche Windows prise : les touches appuyées avec elle (Win+L, Win+flèche…) le sont aussi,
        // sans quoi Windows ferait le raccourci ici.
        let with_super = self.pressed.contains_key(&VK_LWIN.0) || self.pressed.contains_key(&VK_RWIN.0);
        let extended = event.flags.0 & LLKHF_EXTENDED.0 != 0;
        let Some(keysym) =
            keys::virtual_key(vk, extended).or_else(|| with_super.then(|| character(vk, shift)).flatten())
        else {
            return false;
        };
        // Combinaisons que Windows garde pour lui : touches spéciales, comme la touche Windows.
        let combination = (alt && (vk == VK_TAB.0 || vk == VK_ESCAPE.0)) || (ctrl && vk == VK_ESCAPE.0);
        let group = if combination {
            Some(Group::Special)
        } else {
            keys::group(keysym)
        };
        // Alt+F4, les touches que la fenêtre ne transmet pas (Menu, Pause) ou qu'elle change en
        // commandes du presse-papiers, celles appuyées avec la touche Windows prise, et selon les
        // réglages les touches spéciales, de volume et multimédia.
        let take = (alt && vk == VK_F4.0)
            || keysym == keys::MENU
            || keysym == keys::PAUSE
            || ((ctrl || shift) && (vk == VK_INSERT.0 || vk == VK_DELETE.0))
            || with_super
            || group.is_some_and(|g| target.settings.sends(g));
        if !take {
            return false;
        }
        // La fenêtre envoie les touches de modification à son rythme : elles partent ici aussi,
        // avant la touche (un second « appuyé » est sans effet).
        for (held, modifier) in [(ctrl, keys::CONTROL_L), (alt, keys::ALT_L), (shift, keys::SHIFT_L)] {
            if held {
                sender.key(true, modifier);
            }
        }
        sender.key(true, keysym);
        self.pressed.insert(vk, keysym);
        true
    }
}

/// Touche de caractère : ce qu'elle produit sans Maj selon la disposition du clavier, une lettre en
/// majuscule avec Maj (comme `keys::printable`).
fn character(vk: u16, shift: bool) -> Option<u32> {
    // SAFETY : simple traduction, sans état. Le bit fort marque une touche morte.
    let code = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_CHAR) } & 0x7fff_ffff;
    let c = char::from_u32(code).filter(|c| *c != '\0' && !c.is_control())?;
    let c = if shift {
        c.to_uppercase().next()
    } else {
        c.to_lowercase().next()
    }?;
    Some(keys::char_keysym(c))
}

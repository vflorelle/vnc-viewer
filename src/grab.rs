//! Clavier sous Linux : le bureau garde pour lui ses raccourcis (touche Windows, Alt+Tab, Alt+F4,
//! Impr. écran, verrouillage…) avant qu'ils n'arrivent à la fenêtre. Une session qui envoie les
//! touches spéciales les lui demande :
//! - sous Wayland, par le protocole d'inhibition des raccourcis, actif tant que sa fenêtre a le
//!   focus (le bureau peut demander une fois l'autorisation) ;
//! - sous X11, en prenant le clavier tant qu'elle a le focus (en réessayant tant que le bureau le
//!   tient encore).
//!
//! La couche des fenêtres ne donne pas la surface Wayland d'une fenêtre de session : un second
//! clavier, ouvert sur la même connexion, apprend par son évènement d'entrée la surface qui a le
//! focus.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use raw_window_handle::{DisplayHandle, HandleError, RawDisplayHandle};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1 as Manager;
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::zwp_keyboard_shortcuts_inhibitor_v1::{
    self as inhibitor, ZwpKeyboardShortcutsInhibitorV1 as Inhibitor,
};
use x11_dl::xlib;

/// Raccourcis du bureau, pour une session (affiché dans ses informations).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Shortcuts {
    /// Ni Wayland avec le protocole d'inhibition, ni X11.
    #[default]
    Unsupported,
    /// Pas demandés : touches spéciales désactivées, session qui n'envoie pas le clavier (lecture
    /// seule, menu ouvert…) ou sans le focus.
    NotRequested,
    /// Demandés, jamais accordés : autorisation attendue ou refusée (Wayland), clavier encore tenu
    /// par le bureau (X11).
    Pending,
    /// Pris : ils partent au serveur.
    Taken,
    /// Accordés puis rendus au bureau, qui les traite de nouveau.
    Returned,
}

pub enum Grab {
    Wayland(Box<Wayland>),
    X11(Box<X11>),
    Unsupported,
}

impl Grab {
    /// `display` : connexion de la couche des fenêtres, ouverte toute la durée du programme.
    pub fn new(display: Result<DisplayHandle<'_>, HandleError>) -> Self {
        let grab = match display.map(|d| d.as_raw()) {
            Ok(RawDisplayHandle::Wayland(h)) => Wayland::new(h.display).map(|w| Grab::Wayland(Box::new(w))),
            Ok(RawDisplayHandle::Xlib(h)) => h.display.and_then(X11::new).map(|x| Grab::X11(Box::new(x))),
            _ => None,
        };
        grab.unwrap_or(Grab::Unsupported)
    }

    /// `focused` : session qui a le focus et envoie les touches spéciales ; `sending` : toutes
    /// celles qui les envoient (les autres rendent ses raccourcis au bureau). Renvoie le délai
    /// après lequel recommencer, si le bureau les a refusés pour l'instant.
    pub fn update(&mut self, focused: Option<u64>, sending: &[u64]) -> Option<Duration> {
        match self {
            Grab::Wayland(w) => {
                w.update(focused, sending);
                None
            }
            Grab::X11(x) => x.update(focused),
            Grab::Unsupported => None,
        }
    }

    pub fn status(&self, id: u64) -> Shortcuts {
        match self {
            Grab::Wayland(w) => w.status(id),
            Grab::X11(x) if x.grabbed == Some(id) => Shortcuts::Taken,
            Grab::X11(x) if x.wanted == Some(id) => Shortcuts::Pending,
            Grab::X11(_) => Shortcuts::NotRequested,
            Grab::Unsupported => Shortcuts::Unsupported,
        }
    }
}

pub struct Wayland {
    conn: Connection,
    queue: EventQueue<Focus>,
    focus: Focus,
    seat: WlSeat,
    manager: Manager,
    /// Session, sa surface et l'inhibiteur posé dessus (un seul par surface).
    inhibitors: Vec<(u64, WlSurface, Inhibitor)>,
    _keyboard: WlKeyboard,
}

/// État d'un inhibiteur, donné par le bureau.
#[derive(Default)]
struct State {
    active: AtomicBool,
    /// Actif au moins une fois : le bureau (ou l'utilisateur) l'a autorisé.
    granted: AtomicBool,
}

/// Surface qui a le focus du clavier.
struct Focus(Option<WlSurface>);

impl Wayland {
    fn new(display: NonNull<c_void>) -> Option<Self> {
        // SAFETY : connexion de la couche des fenêtres, valable toute la durée du programme.
        let backend = unsafe { wayland_backend::client::Backend::from_foreign_display(display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, queue) = registry_queue_init::<Focus>(&conn).ok()?;
        let handle = queue.handle();
        let seat: WlSeat = globals.bind(&handle, 1..=1, ()).ok()?;
        let manager: Manager = globals.bind(&handle, 1..=1, ()).ok()?;
        let keyboard = seat.get_keyboard(&handle, ());
        Some(Wayland {
            conn,
            queue,
            focus: Focus(None),
            seat,
            manager,
            inhibitors: Vec::new(),
            _keyboard: keyboard,
        })
    }

    fn update(&mut self, focused: Option<u64>, sending: &[u64]) {
        // Évènements déjà lus par la couche des fenêtres (focus, état des inhibiteurs).
        let _ = self.queue.dispatch_pending(&mut self.focus);
        self.inhibitors.retain(|(id, _, inhibitor)| {
            let keep = sending.contains(id);
            if !keep {
                inhibitor.destroy();
            }
            keep
        });
        if let Some(id) = focused
            && let Some(surface) = &self.focus.0
            && !self.inhibitors.iter().any(|(i, s, _)| *i == id || s == surface)
        {
            let handle = self.queue.handle();
            let inhibitor = self
                .manager
                .inhibit_shortcuts(surface, &self.seat, &handle, State::default());
            self.inhibitors.push((id, surface.clone(), inhibitor));
        }
        let _ = self.conn.flush();
    }

    fn status(&self, id: u64) -> Shortcuts {
        let inhibitor = self
            .inhibitors
            .iter()
            .find(|(i, ..)| *i == id)
            .map(|(.., inhibitor)| inhibitor);
        match inhibitor.and_then(|i| i.data::<State>()) {
            None => Shortcuts::NotRequested,
            Some(s) if s.active.load(Ordering::Relaxed) => Shortcuts::Taken,
            Some(s) if s.granted.load(Ordering::Relaxed) => Shortcuts::Returned,
            Some(_) => Shortcuts::Pending,
        }
    }
}

impl Dispatch<Inhibitor, State> for Focus {
    fn event(
        _: &mut Self,
        _: &Inhibitor,
        event: inhibitor::Event,
        state: &State,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let active = matches!(event, inhibitor::Event::Active);
        state.active.store(active, Ordering::Relaxed);
        if active {
            state.granted.store(true, Ordering::Relaxed);
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Focus {
    fn event(
        focus: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } => focus.0 = Some(surface),
            wl_keyboard::Event::Leave { .. } => focus.0 = None,
            _ => {}
        }
    }
}

/// Objets sans évènement utile ici.
macro_rules! ignore_events {
    ($($object:ty, $data:ty);*) => {$(
        impl Dispatch<$object, $data> for Focus {
            fn event(_: &mut Self, _: &$object, _: <$object as Proxy>::Event, _: &$data, _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(WlRegistry, GlobalListContents; WlSeat, (); Manager, ());

pub struct X11 {
    xlib: xlib::Xlib,
    display: *mut xlib::Display,
    /// Session qui a le focus et demande le clavier.
    wanted: Option<u64>,
    /// Session pour laquelle le clavier est pris.
    grabbed: Option<u64>,
    /// Dernière tentative pour la session demandeuse.
    tried: Option<Instant>,
}

/// Intervalle des tentatives : le clavier peut être encore tenu par le bureau quand la session
/// reçoit le focus (sélecteur de fenêtres de `Alt+Tab` jusqu'au relâchement d'`Alt`).
const RETRY: Duration = Duration::from_millis(200);

impl X11 {
    fn new(display: NonNull<c_void>) -> Option<Self> {
        Some(X11 {
            xlib: xlib::Xlib::open().ok()?,
            display: display.as_ptr().cast(),
            wanted: None,
            grabbed: None,
            tried: None,
        })
    }

    /// Prend le clavier pour la fenêtre qui a le focus (une session), ou le rend. Renvoie le délai
    /// avant la tentative suivante, si celle-ci a échoué.
    fn update(&mut self, focused: Option<u64>) -> Option<Duration> {
        // SAFETY : connexion de la couche des fenêtres, utilisée depuis le même thread qu'elle.
        unsafe {
            if focused != self.wanted {
                self.wanted = focused;
                self.tried = None;
                if self.grabbed.take().is_some() {
                    (self.xlib.XUngrabKeyboard)(self.display, xlib::CurrentTime);
                    (self.xlib.XFlush)(self.display);
                }
            }
            let id = focused?;
            if self.grabbed.is_some() {
                return None;
            }
            if self.tried.is_none_or(|t| t.elapsed() >= RETRY) {
                self.tried = Some(Instant::now());
                let (mut window, mut revert) = (0, 0);
                (self.xlib.XGetInputFocus)(self.display, &mut window, &mut revert);
                // 0 et 1 : aucune fenêtre, ou celle sous la souris.
                if window > 1
                    && (self.xlib.XGrabKeyboard)(
                        self.display,
                        window,
                        xlib::True,
                        xlib::GrabModeAsync,
                        xlib::GrabModeAsync,
                        xlib::CurrentTime,
                    ) == xlib::GrabSuccess
                {
                    self.grabbed = Some(id);
                    (self.xlib.XFlush)(self.display);
                    return None;
                }
            }
            Some(RETRY)
        }
    }
}

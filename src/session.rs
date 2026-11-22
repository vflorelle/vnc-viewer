//! Une session VNC : connexion et lecture dans un thread, écriture dans un autre ; état partagé
//! avec l'interface (statut, image, curseur, questions à l'utilisateur).

use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use eframe::egui;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::config::{Quality, Settings};
use crate::decode::{self, Change, Cursor, Decoder, Framebuffer};
use crate::rfb::{self, Address, Auth, Conn, PixelFormat, ReadExt, SecurityInfo};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Taille maximale d'un presse-papiers reçu.
const MAX_CUT_TEXT: usize = 64 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Connecting,
    /// En attente de l'utilisateur ou du serveur pendant l'authentification.
    Authenticating,
    Connected,
    Closed(Closed),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Closed {
    pub message: String,
    /// Mot de passe refusé : le redemander à la prochaine connexion.
    pub auth_failed: bool,
}

/// Question posée à l'utilisateur pendant la négociation.
#[derive(Clone, Debug, PartialEq)]
pub enum Prompt {
    Credentials {
        username: bool,
    },
    /// Clé du serveur jamais vue (`known` vide) ou différente de celle acceptée avant.
    Fingerprint {
        fingerprint: String,
        known: String,
    },
}

pub enum Answer {
    Credentials {
        user: String,
        password: String,
        remember: bool,
    },
    Accept,
    Cancel,
}

pub struct State {
    pub status: Status,
    /// Nom du bureau annoncé par le serveur.
    pub name: String,
    pub security: Option<SecurityInfo>,
    /// Image affichée ; sa zone `dirty` reste à envoyer à la carte graphique.
    pub fb: Framebuffer,
    pub cursor: Option<Cursor>,
    pub prompt: Option<Prompt>,
    /// Presse-papiers reçu du serveur, à copier sur ce PC.
    pub clipboard_in: Option<String>,
    pub bell: bool,
    /// Identifiants à mémoriser (« Se souvenir »), une fois acceptés par le serveur.
    pub save_credentials: Option<(String, String)>,
    /// Empreinte acceptée par l'utilisateur, à enregistrer.
    pub save_fingerprint: Option<String>,
    /// Presse-papiers de ce PC, fourni au serveur sur demande (presse-papiers étendu).
    local_clipboard: String,
    /// Qualité à appliquer à la fin de la mise à jour en cours.
    quality: Option<Quality>,
    /// Presse-papiers partagé (réglable pendant la session).
    clipboard: bool,
    /// Détails de la session : fenêtre d'informations, extensions gérées par le serveur.
    pub info: Info,
}

/// Détails techniques de la session.
#[derive(Clone, Default)]
pub struct Info {
    /// Protocole retenu, et celui annoncé par le serveur.
    pub protocol: String,
    pub server_protocol: String,
    pub format: Option<PixelFormat>,
    /// Encodages reçus jusqu'ici.
    pub encodings: Vec<i32>,
    pub updates: u64,
    /// Octets reçus du serveur.
    pub received: u64,
    /// Le serveur gère les boutons de souris étendus (bouton « suivant »).
    pub extended_mouse: bool,
    /// Le serveur gère le presse-papiers étendu (texte Unicode).
    pub extended_clipboard: bool,
}

pub fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// Paramètres d'une connexion.
pub struct Params {
    pub address: Address,
    pub settings: Settings,
    pub username: String,
    /// Mot de passe mémorisé, essayé sans rien demander.
    pub password: Option<String>,
    /// Empreinte de la clé du serveur acceptée auparavant.
    pub fingerprint: String,
}

pub struct Session {
    pub state: Arc<Mutex<State>>,
    out: Sender<Vec<u8>>,
    answers: Sender<Answer>,
    socket: Arc<Mutex<Option<TcpStream>>>,
}

impl Session {
    /// Lance la connexion ; `repaint` redessine la fenêtre de la session.
    pub fn start(params: Params, repaint: impl Fn() + Send + Sync + 'static) -> Session {
        let state = Arc::new(Mutex::new(State {
            status: Status::Connecting,
            name: String::new(),
            security: None,
            fb: Framebuffer::default(),
            cursor: None,
            prompt: None,
            clipboard_in: None,
            bell: false,
            save_credentials: None,
            save_fingerprint: None,
            local_clipboard: String::new(),
            quality: None,
            clipboard: params.settings.clipboard,
            info: Info::default(),
        }));
        let (out, out_rx) = channel();
        let (answers, answers_rx) = channel();
        let socket = Arc::new(Mutex::new(None));
        let session = Session {
            state: state.clone(),
            out: out.clone(),
            answers,
            socket: socket.clone(),
        };
        std::thread::spawn(move || {
            let repaint = Arc::new(repaint);
            let result = run(&params, &state, out, out_rx, answers_rx, &socket, repaint.clone());
            // Le thread d'écriture s'arrête aussi.
            if let Some(socket) = socket.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = socket.shutdown(Shutdown::Both);
            }
            let closed = match result {
                Ok(()) => Closed {
                    message: tr!("Connexion fermée par le serveur", "Connection closed by the server"),
                    auth_failed: false,
                },
                Err(e) => Closed {
                    auth_failed: e.kind() == rfb::AUTH_FAILED,
                    message: match e.kind() {
                        io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset => {
                            tr!("Connexion perdue", "Connection lost")
                        }
                        _ => capitalize(&e.to_string()),
                    },
                },
            };
            let mut st = lock(&state);
            // Une fermeture demandée (Session::close) a déjà mis son statut.
            if !matches!(st.status, Status::Closed(_)) {
                st.status = Status::Closed(closed);
            }
            st.prompt = None;
            drop(st);
            repaint();
        });
        session
    }

    pub fn answer(&self, answer: Answer) {
        lock(&self.state).prompt = None;
        let _ = self.answers.send(answer);
    }

    fn send(&self, msg: Vec<u8>) {
        let _ = self.out.send(msg);
    }

    pub fn key(&self, down: bool, keysym: u32) {
        self.send(rfb::key_event(down, keysym));
    }

    #[cfg(windows)]
    pub fn keys(&self) -> Keys {
        Keys(self.out.clone())
    }

    /// `buttons` : masque des boutons 1 à 9 ; le 9 (« suivant ») n'existe qu'avec les boutons
    /// étendus, que le serveur annonce.
    pub fn pointer(&self, buttons: u16, x: u16, y: u16) {
        let extended = lock(&self.state).info.extended_mouse;
        self.send(rfb::pointer_event(buttons, x, y, extended));
    }

    pub fn ctrl_alt_del(&self) {
        use crate::keys::{ALT_L, CONTROL_L, DELETE};
        for (down, key) in [
            (true, CONTROL_L),
            (true, ALT_L),
            (true, DELETE),
            (false, DELETE),
            (false, ALT_L),
            (false, CONTROL_L),
        ] {
            self.key(down, key);
        }
    }

    /// Envoie le presse-papiers de ce PC au serveur.
    pub fn clipboard(&self, text: &str) {
        let mut st = lock(&self.state);
        if st.info.extended_clipboard {
            st.local_clipboard = text.to_owned();
            drop(st);
            self.send(rfb::client_cut_text_extended(&clip::NOTIFY_TEXT.to_be_bytes()));
        } else {
            drop(st);
            self.send(rfb::client_cut_text(text));
        }
    }

    /// Redemande tout l'écran.
    pub fn refresh(&self) {
        let st = lock(&self.state);
        let (w, h) = (st.fb.width as u16, st.fb.height as u16);
        drop(st);
        self.send(rfb::update_request(false, w, h));
    }

    /// Change la qualité d'image (appliquée entre deux mises à jour) et le partage du
    /// presse-papiers.
    pub fn set_options(&self, quality: Quality, clipboard: bool) {
        let mut st = lock(&self.state);
        st.quality = Some(quality);
        st.clipboard = clipboard;
        drop(st);
        // Les encodages peuvent changer tout de suite (le format des pixels attend).
        self.send(rfb::set_encodings(&encodings(quality, clipboard)));
    }

    /// Ferme la connexion (et débloque une question en attente).
    pub fn close(&self) {
        {
            let mut st = lock(&self.state);
            if !matches!(st.status, Status::Closed(_)) {
                st.status = Status::Closed(Closed {
                    message: tr!("Déconnecté", "Disconnected"),
                    auth_failed: false,
                });
            }
        }
        let _ = self.answers.send(Answer::Cancel);
        if let Some(socket) = self.socket.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

pub fn pixel_format(quality: Quality) -> PixelFormat {
    match quality {
        Quality::Auto | Quality::High => PixelFormat::RGB888,
        Quality::Medium => PixelFormat::RGB565,
        Quality::Low => PixelFormat::BGR233,
    }
}

/// Encodages proposés, du préféré au moins bon, puis les pseudo-encodages.
pub fn encodings(quality: Quality, clipboard: bool) -> Vec<i32> {
    use decode::*;
    let mut list = vec![COPY_RECT, TIGHT, ZRLE, HEXTILE, ZLIB, RRE, RAW];
    // Sans qualité JPEG annoncée, Tight reste sans perte.
    match quality {
        Quality::Auto => list.extend([JPEG_QUALITY_0 + 8, COMPRESS_LEVEL_0 + 2]),
        Quality::High => list.push(COMPRESS_LEVEL_0 + 2),
        Quality::Medium => list.extend([JPEG_QUALITY_0 + 6, COMPRESS_LEVEL_0 + 6]),
        Quality::Low => list.extend([JPEG_QUALITY_0 + 3, COMPRESS_LEVEL_0 + 9]),
    }
    list.extend([
        CURSOR_WITH_ALPHA,
        CURSOR,
        DESKTOP_SIZE,
        EXTENDED_DESKTOP_SIZE,
        LAST_RECT,
        DESKTOP_NAME,
        EXTENDED_MOUSE_BUTTONS,
    ]);
    if clipboard {
        list.push(EXTENDED_CLIPBOARD);
    }
    list
}

/// Questions de la négociation, posées dans la fenêtre de la session.
struct UiAuth<'a> {
    state: &'a Mutex<State>,
    answers: &'a Receiver<Answer>,
    repaint: &'a dyn Fn(),
    username: String,
    password: Option<String>,
    known: String,
    /// Identifiants à mémoriser si le serveur les accepte.
    remember: Option<(String, String)>,
}

impl UiAuth<'_> {
    fn ask(&self, prompt: Prompt) -> Answer {
        {
            let mut st = lock(self.state);
            if matches!(st.status, Status::Closed(_)) {
                return Answer::Cancel;
            }
            st.status = Status::Authenticating;
            st.prompt = Some(prompt);
        }
        (self.repaint)();
        self.answers.recv().unwrap_or(Answer::Cancel)
    }
}

impl Auth for UiAuth<'_> {
    fn credentials(&mut self, username: bool) -> Option<(String, String)> {
        if let Some(password) = self.password.take()
            && (!username || !self.username.is_empty())
        {
            return Some((self.username.clone(), password));
        }
        match self.ask(Prompt::Credentials { username }) {
            Answer::Credentials {
                user,
                password,
                remember,
            } => {
                if remember {
                    self.remember = Some((user.clone(), password.clone()));
                }
                Some((user, password))
            }
            _ => None,
        }
    }

    fn verify(&mut self, fingerprint: &str) -> bool {
        if fingerprint == self.known {
            return true;
        }
        let prompt = Prompt::Fingerprint {
            fingerprint: fingerprint.to_owned(),
            known: self.known.clone(),
        };
        match self.ask(prompt) {
            Answer::Accept => {
                lock(self.state).save_fingerprint = Some(fingerprint.to_owned());
                true
            }
            _ => false,
        }
    }
}

fn connect(address: &Address) -> io::Result<TcpStream> {
    let mut last = None;
    for addr in (address.host.as_str(), address.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, tr!("adresse introuvable", "address not found"))))
}

fn run(
    params: &Params,
    state: &Mutex<State>,
    out: Sender<Vec<u8>>,
    out_rx: Receiver<Vec<u8>>,
    answers: Receiver<Answer>,
    socket: &Mutex<Option<TcpStream>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
) -> io::Result<()> {
    let stream = connect(&params.address)?;
    let _ = stream.set_nodelay(true);
    *socket.lock().unwrap_or_else(|e| e.into_inner()) = Some(stream.try_clone()?);
    if matches!(lock(state).status, Status::Closed(_)) {
        // Fermée pendant la connexion.
        return Ok(());
    }
    let mut conn = Conn {
        input: Box::new(BufReader::with_capacity(1 << 16, stream.try_clone()?)),
        output: Box::new(stream),
    };
    lock(state).status = Status::Authenticating;
    let mut auth = UiAuth {
        state,
        answers: &answers,
        repaint: &*repaint,
        username: params.username.clone(),
        password: params.password.clone().filter(|p| !p.is_empty()),
        known: params.fingerprint.clone(),
        remember: None,
    };
    let init = rfb::handshake(&mut conn, params.settings.encryption, params.settings.shared, &mut auth)?;

    let quality = params.settings.quality;
    let format = pixel_format(quality);
    let (width, height) = (usize::from(init.width), usize::from(init.height));
    let mut fb = Framebuffer::new(width, height);
    {
        let mut st = lock(state);
        st.status = Status::Connected;
        st.name = init.name;
        st.security = Some(init.security);
        st.fb = fb.clone();
        st.save_credentials = auth.remember.take();
        st.info.format = Some(format);
        st.info.protocol = init.version;
        st.info.server_protocol = init.server_version;
    }
    repaint();

    // Écriture dans son propre thread : l'interface n'attend jamais le réseau.
    let mut output = conn.output;
    std::thread::spawn(move || {
        for msg in out_rx {
            if output.write_all(&msg).and_then(|()| output.flush()).is_err() {
                break;
            }
        }
    });
    let send = |msg: Vec<u8>| out.send(msg).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe));
    send(rfb::set_pixel_format(format))?;
    send(rfb::set_encodings(&encodings(quality, params.settings.clipboard)))?;
    send(rfb::update_request(false, init.width, init.height))?;

    let mut input = Counted {
        inner: conn.input,
        count: 0,
    };
    let mut decoder = Decoder::new(format);
    loop {
        match input.u8()? {
            0 => {
                input.skip(1)?;
                let count = input.u16()?;
                let mut full = false;
                let mut cursor = None;
                let mut name = None;
                let mut extended_mouse = false;
                let mut used = Vec::new();
                for _ in 0..count {
                    let x = usize::from(input.u16()?);
                    let y = usize::from(input.u16()?);
                    let w = usize::from(input.u16()?);
                    let h = usize::from(input.u16()?);
                    let encoding = input.i32()?;
                    if encoding >= 0 && !used.contains(&encoding) {
                        used.push(encoding);
                    }
                    match decoder.rect(&mut input, &mut fb, (x, y, w, h), encoding)? {
                        Change::Pixels => {}
                        Change::Resized => full = true,
                        Change::Cursor(c) => cursor = Some(c),
                        Change::Name(n) => name = Some(n),
                        Change::ExtendedMouse => extended_mouse = true,
                        Change::LastRect => break,
                    }
                }
                let mut st = lock(state);
                fb.flush_into(&mut st.fb);
                if cursor.is_some() {
                    st.cursor = cursor;
                }
                if let Some(n) = name {
                    st.name = n;
                }
                if extended_mouse {
                    st.info.extended_mouse = true;
                }
                for e in used {
                    if !st.info.encodings.contains(&e) {
                        st.info.encodings.push(e);
                    }
                }
                st.info.updates += 1;
                st.info.received = input.count;
                let quality = st.quality.take();
                drop(st);
                if let Some(quality) = quality {
                    // Entre deux mises à jour : aucun pixel de l'ancien format n'est en route.
                    let format = pixel_format(quality);
                    send(rfb::set_pixel_format(format))?;
                    decoder.set_format(format);
                    lock(state).info.format = Some(format);
                    full = true;
                }
                send(rfb::update_request(!full, fb.width as u16, fb.height as u16))?;
                repaint();
            }
            1 => {
                // Palette : inutile, les couleurs vraies sont demandées.
                input.skip(1)?;
                let _first = input.u16()?;
                let count = input.u16()?;
                input.skip(usize::from(count) * 6)?;
            }
            2 => {
                lock(state).bell = true;
                repaint();
            }
            3 => {
                input.skip(3)?;
                let len = input.i32()?;
                let size = len.unsigned_abs() as usize;
                if size > MAX_CUT_TEXT {
                    return Err(rfb::protocol_error(tr!(
                        "presse-papiers trop gros ({size} octets)",
                        "clipboard too large ({size} bytes)"
                    )));
                }
                let data = input.read_vec(size)?;
                if !lock(state).clipboard {
                    continue;
                }
                if len >= 0 {
                    let text: String = data.iter().map(|&b| char::from(b)).collect();
                    lock(state).clipboard_in = Some(text);
                    repaint();
                } else if let Some(reply) = clip::handle(state, &data) {
                    send(reply)?;
                    repaint();
                }
            }
            other => {
                return Err(rfb::protocol_error(tr!(
                    "message inattendu du serveur ({other})",
                    "unexpected message from the server ({other})"
                )));
            }
        }
    }
}

/// Presse-papiers étendu : texte Unicode, échangé à la demande.
mod clip {
    use super::*;

    pub const TEXT: u32 = 1;
    pub const CAPS: u32 = 1 << 24;
    pub const REQUEST: u32 = 1 << 25;
    pub const PEEK: u32 = 1 << 26;
    pub const NOTIFY: u32 = 1 << 27;
    pub const PROVIDE: u32 = 1 << 28;
    pub const NOTIFY_TEXT: u32 = NOTIFY | TEXT;
    /// Taille maximale d'un texte envoyé sans demande.
    const MAX_TEXT: u32 = 10 << 20;

    fn message(flags: u32, data: &[u8]) -> Vec<u8> {
        let mut payload = flags.to_be_bytes().to_vec();
        payload.extend_from_slice(data);
        rfb::client_cut_text_extended(&payload)
    }

    /// Réponse éventuelle à un message du serveur.
    pub fn handle(state: &Mutex<State>, data: &[u8]) -> Option<Vec<u8>> {
        let flags = u32::from_be_bytes(data.get(..4)?.try_into().ok()?);
        let body = &data[4..];
        if flags & CAPS != 0 {
            lock(state).info.extended_clipboard = true;
            let flags = CAPS | REQUEST | PEEK | NOTIFY | PROVIDE | TEXT;
            return Some(message(flags, &MAX_TEXT.to_be_bytes()));
        }
        if flags & (REQUEST | TEXT) == REQUEST | TEXT {
            let text = lock(state).local_clipboard.replace("\r\n", "\n").replace('\n', "\r\n");
            let mut plain = (text.len() as u32 + 1).to_be_bytes().to_vec();
            plain.extend_from_slice(text.as_bytes());
            plain.push(0);
            let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
            z.write_all(&plain).ok()?;
            return Some(message(PROVIDE | TEXT, &z.finish().ok()?));
        }
        if flags & PEEK != 0 {
            return Some(message(NOTIFY_TEXT, &[]));
        }
        if flags & NOTIFY != 0 && flags & TEXT != 0 {
            return Some(message(REQUEST | TEXT, &[]));
        }
        if flags & (PROVIDE | TEXT) == PROVIDE | TEXT {
            // Les formats se suivent dans l'ordre de leurs bits ; le texte est le premier.
            let mut plain = Vec::new();
            ZlibDecoder::new(body)
                .take(MAX_TEXT as u64 + 4)
                .read_to_end(&mut plain)
                .ok()?;
            let len = u32::from_be_bytes(plain.get(..4)?.try_into().ok()?) as usize;
            let text = plain.get(4..4 + len)?;
            let text = String::from_utf8_lossy(text);
            let text = text.trim_end_matches('\0').replace("\r\n", "\n");
            lock(state).clipboard_in = Some(text);
        }
        None
    }
}

/// Envoi des touches, utilisable hors de l'interface (crochet clavier de Windows).
#[cfg(windows)]
#[derive(Clone)]
pub struct Keys(Sender<Vec<u8>>);

#[cfg(windows)]
impl Keys {
    pub fn key(&self, down: bool, keysym: u32) {
        let _ = self.0.send(rfb::key_event(down, keysym));
    }
}

/// Bouton de la souris dans le masque RFB.
pub fn button_mask(button: egui::PointerButton) -> u16 {
    match button {
        egui::PointerButton::Primary => 1,
        egui::PointerButton::Middle => 2,
        egui::PointerButton::Secondary => 4,
        egui::PointerButton::Extra1 => 1 << 7,
        egui::PointerButton::Extra2 => 1 << 8,
    }
}

/// Flux entrant qui compte les octets reçus.
struct Counted {
    inner: Box<dyn Read + Send>,
    count: u64,
}

impl Read for Counted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        Ok(n)
    }
}

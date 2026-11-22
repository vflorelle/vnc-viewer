//! Fenêtre d'une session : écran distant, souris et clavier, barre d'outils, menu F8, questions
//! de la connexion (mot de passe, identité du serveur).

use std::time::Duration;

use eframe::egui::{
    self, Color32, ColorImage, CursorIcon, Event, Key, Modifiers, MouseWheelUnit, Pos2, Rect, RichText, TextureHandle,
    TextureOptions, Vec2, ViewportBuilder, ViewportCommand, ViewportId,
};

use crate::config::{Connection, Quality, Scaling, Settings};
use crate::keys;
use crate::rfb::SecurityInfo;
use crate::session::{self, Answer, Prompt, Session, Status, lock};

/// Délai entre deux lectures du presse-papiers de ce PC (fenêtre active).
const CLIPBOARD_POLL: Duration = Duration::from_secs(1);
/// Points de défilement (pavé tactile) pour un cran de molette.
const POINTS_PER_NOTCH: f32 = 50.0;

/// Ce que la fenêtre demande à l'application.
#[derive(Default)]
pub struct Output {
    pub close: bool,
    pub reconnect: bool,
    /// Réglages changés (barre d'outils, menu), à enregistrer dans le carnet.
    pub settings_changed: bool,
    pub credentials: Option<(String, String)>,
    pub fingerprint: Option<String>,
}

pub struct View {
    /// Entrée du carnet d'adresses.
    pub id: u64,
    pub viewport: ViewportId,
    pub session: Session,
    pub settings: Settings,
    label: String,
    texture: Option<TextureHandle>,
    fullscreen: bool,
    /// Plein écran demandé à l'ouverture de la fenêtre.
    start_fullscreen: bool,
    menu: bool,
    user: String,
    password: String,
    remember: bool,
    /// Boutons de souris appuyés (masque RFB) et dernière position envoyée.
    buttons: u16,
    pointer: Option<(u16, u16)>,
    wheel: Vec2,
    /// Fenêtre déjà dimensionnée d'après l'écran distant.
    sized: bool,
    /// Hauteur de la barre d'outils (dimensions de la fenêtre).
    toolbar_height: f32,
    /// La fenêtre de la session a le focus (elle reçoit alors le clavier).
    focused: bool,
    /// Raccourcis du bureau (Linux), tenus à jour par la fenêtre principale.
    #[cfg(target_os = "linux")]
    pub shortcuts: crate::grab::Shortcuts,
    /// Dernier texte échangé par le presse-papiers (pour ne pas le renvoyer).
    clip: Option<String>,
    clip_checked: f64,
    keyboard: Keyboard,
    /// Curseur du serveur et sa texture, dessiné au-dessus de l'écran distant.
    cursor: Option<(crate::decode::Cursor, TextureHandle)>,
    /// Adresse du serveur (fenêtre d'informations).
    address: String,
    /// Question affichée à l'image précédente (le focus va au premier champ à remplir).
    had_prompt: bool,
    info: bool,
    /// Réglages en cours de modification (fenêtre Propriétés).
    properties: Option<Settings>,
    /// Mesure du débit : instant et octets reçus de la dernière mesure, débit en octets/s.
    rate: (f64, u64, f64),
}

impl View {
    pub fn new(conn: &Connection, session: Session, viewport: ViewportId) -> Self {
        View {
            id: conn.id,
            viewport,
            session,
            settings: conn.settings.clone(),
            label: conn.label().to_owned(),
            texture: None,
            fullscreen: conn.settings.fullscreen,
            start_fullscreen: conn.settings.fullscreen,
            menu: false,
            user: conn.username.clone(),
            password: String::new(),
            remember: false,
            buttons: 0,
            pointer: None,
            wheel: Vec2::ZERO,
            sized: false,
            toolbar_height: 0.0,
            focused: false,
            #[cfg(target_os = "linux")]
            shortcuts: Default::default(),
            clip: None,
            clip_checked: 0.0,
            keyboard: Keyboard::default(),
            cursor: None,
            address: conn.address.clone(),
            had_prompt: false,
            info: false,
            properties: None,
            rate: (0.0, 0, 0.0),
        }
    }

    /// Nouvelle session dans la même fenêtre (reconnexion).
    pub fn replace_session(&mut self, conn: &Connection, session: Session) {
        self.session = session;
        self.label = conn.label().to_owned();
        // Nom et mot de passe tapés gardés : un mot de passe refusé se corrige sans tout retaper.
        if self.user.is_empty() {
            self.user = conn.username.clone();
        }
        self.buttons = 0;
        self.pointer = None;
        self.keyboard = Keyboard::default();
        self.cursor = None;
        self.address = conn.address.clone();
        self.rate = (0.0, 0, 0.0);
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    /// Menu F8 ou propriétés ouverts : le clavier reste à la visionneuse (la fenêtre
    /// d'informations, sans saisie, le laisse au serveur).
    fn dialog_open(&self) -> bool {
        self.menu || self.properties.is_some()
    }

    /// La fenêtre envoie le clavier et la souris au serveur.
    pub fn capturing(&self) -> bool {
        let st = lock(&self.session.state);
        st.status == Status::Connected && st.prompt.is_none() && !self.settings.view_only && !self.dialog_open()
    }

    pub fn show(&mut self, ctx: &egui::Context, clipboard: &mut Option<arboard::Clipboard>) -> Output {
        let name = lock(&self.session.state).name.clone();
        let title = match name.trim() {
            "" => format!("{} — {}", self.label, crate::app::APP_NAME),
            name if name == self.label => format!("{name} — {}", crate::app::APP_NAME),
            name => format!("{} ({name}) — {}", self.label, crate::app::APP_NAME),
        };
        let builder = ViewportBuilder::default()
            .with_title(title)
            // Même programme pour le bureau que la fenêtre principale (autorisations, regroupement).
            .with_app_id(crate::app::APP_NAME)
            .with_inner_size([1024.0, 768.0])
            .with_min_inner_size([320.0, 240.0])
            .with_fullscreen(self.start_fullscreen);
        let mut output = Output::default();
        ctx.show_viewport_immediate(self.viewport, builder, |ui, _| self.ui(ui, &mut output, clipboard));
        self.properties_window(ctx, &mut output);
        output
    }

    fn ui(&mut self, ui: &mut egui::Ui, out: &mut Output, clipboard: &mut Option<arboard::Clipboard>) {
        let ctx = ui.ctx().clone();
        let (focused, close, now) = ui.input(|i| {
            (
                i.viewport().focused == Some(true),
                i.viewport().close_requested(),
                i.time,
            )
        });
        if close {
            out.close = true;
        }
        let gained_focus = focused && !self.focused;
        self.focused = focused;

        // État de la session.
        let (status, prompt, security, cursor, clip_in, bell) = {
            let state = self.session.state.clone();
            let mut st = lock(&state);
            out.credentials = st.save_credentials.take();
            out.fingerprint = st.save_fingerprint.take();
            self.upload(&ctx, &mut st.fb);
            (
                st.status.clone(),
                st.prompt.clone(),
                st.security,
                st.cursor.clone(),
                st.clipboard_in.take(),
                std::mem::take(&mut st.bell),
            )
        };
        let connected = status == Status::Connected;
        if connected && !self.password.is_empty() {
            // Accepté : inutile de le garder (il est mémorisé à part si demandé).
            self.password.clear();
        }
        if let Some(text) = clip_in {
            ctx.copy_text(text.clone());
            self.clip = Some(text);
        }
        if bell {
            beep(&ctx, focused);
        }
        if connected && self.settings.clipboard && focused {
            if gained_focus || now - self.clip_checked >= CLIPBOARD_POLL.as_secs_f64() {
                self.clip_checked = now;
                if let Some(text) = clipboard.as_mut().and_then(|c| c.get_text().ok())
                    && self.clip.as_ref() != Some(&text)
                {
                    self.session.clipboard(&text);
                    self.clip = Some(text);
                }
            }
            ctx.request_repaint_after(CLIPBOARD_POLL);
        }
        if !focused && self.buttons != 0 {
            // Boutons relâchés hors de la fenêtre.
            self.buttons = 0;
            if let Some((x, y)) = self.pointer {
                self.session.pointer(0, x, y);
            }
        }
        if !focused || !connected {
            self.keyboard.release(&self.session);
        }

        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F8)) && connected {
            self.menu = !self.menu;
        }
        let menu_step = if self.menu { menu_keys(ui) } else { 0 };
        if !self.fullscreen && self.settings.toolbar {
            let bar = egui::Panel::top("barre").show(ui, |ui| self.toolbar(ui, security, out));
            self.toolbar_height = bar.response.rect.height();
        }
        if connected && !self.sized {
            self.fit_window(ui);
        }
        let frame = egui::Frame::central_panel(ui.style())
            .fill(Color32::BLACK)
            .inner_margin(0.0);
        let first_frame = prompt.is_some() && !self.had_prompt;
        self.had_prompt = prompt.is_some();
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            if connected && self.texture.is_some() {
                self.screen(ui, cursor.as_ref());
            }
            match (&status, prompt) {
                (_, Some(prompt)) => self.prompt(ui, &prompt, first_frame),
                (Status::Closed(closed), _) => {
                    let message = closed.message.clone();
                    overlay(ui, |ui| {
                        ui.label(RichText::new(message).strong());
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            out.reconnect = ui.button(tr!("Se reconnecter", "Reconnect")).clicked();
                            out.close |= ui.button(tr!("Fermer", "Close")).clicked();
                        });
                    });
                }
                (Status::Connected, None) => {}
                (_, None) => {
                    let label = self.label.clone();
                    overlay(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(tr!("Connexion à {label}…", "Connecting to {label}…"));
                        });
                        ui.add_space(8.0);
                        out.close |= ui.button(tr!("Annuler", "Cancel")).clicked();
                    });
                }
            }
        });
        if self.fullscreen && connected {
            self.fullscreen_bar(ui, security, out);
        }
        if self.menu {
            self.f8_menu(ui, out, menu_step);
        }
        if self.info {
            self.info_window(ui, security);
        }
        if connected && !self.dialog_open() && !self.settings.view_only {
            // Aucun bouton de l'interface ne garde le clavier : il va au serveur.
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
            self.keyboard.events(ui, &self.session, &self.settings);
        }
    }

    /// Envoie la zone modifiée de l'image à la carte graphique.
    fn upload(&mut self, ctx: &egui::Context, fb: &mut crate::decode::Framebuffer) {
        if fb.width == 0 {
            return;
        }
        let options = match self.settings.scaling {
            Scaling::Fit => TextureOptions::LINEAR,
            Scaling::Native => TextureOptions::NEAREST,
        };
        let size = [fb.width, fb.height];
        match &mut self.texture {
            Some(texture) if texture.size() == size => {
                let Some([x0, y0, x1, y1]) = fb.dirty.take() else {
                    return;
                };
                let mut pixels = Vec::with_capacity((x1 - x0) * (y1 - y0));
                for row in y0..y1 {
                    pixels.extend_from_slice(&fb.pixels[row * fb.width + x0..row * fb.width + x1]);
                }
                texture.set_partial([x0, y0], ColorImage::new([x1 - x0, y1 - y0], pixels), options);
            }
            _ => {
                fb.dirty = None;
                let image = ColorImage::new(size, fb.pixels.clone());
                self.texture = Some(ctx.load_texture("ecran", image, options));
            }
        }
    }

    /// Fenêtre à la taille de l'écran distant, sans dépasser celui de ce PC.
    fn fit_window(&mut self, ui: &egui::Ui) {
        let Some(texture) = &self.texture else { return };
        self.sized = true;
        if self.fullscreen {
            return;
        }
        let ppp = ui.ctx().pixels_per_point();
        let [w, h] = texture.size().map(|s| s as f32 / ppp);
        let bar = if self.settings.toolbar {
            self.toolbar_height
        } else {
            0.0
        };
        let mut size = Vec2::new(w, h + bar);
        if let Some(monitor) = ui.input(|i| i.viewport().monitor_size) {
            size = size.min(monitor * 0.9);
        }
        ui.ctx().send_viewport_cmd(ViewportCommand::InnerSize(size));
    }

    fn screen(&mut self, ui: &mut egui::Ui, cursor: Option<&crate::decode::Cursor>) {
        let Some(texture) = self.texture.clone() else { return };
        let ppp = ui.ctx().pixels_per_point();
        let native = Vec2::new(texture.size()[0] as f32, texture.size()[1] as f32) / ppp;
        let rect = match self.settings.scaling {
            Scaling::Fit => {
                let area = ui.max_rect();
                let mut scale = (area.width() / native.x).min(area.height() / native.y);
                // À peine plus grande que la taille réelle : la taille réelle, nette.
                if (1.0..1.02).contains(&scale) {
                    scale = 1.0;
                }
                let size = native * scale;
                let rect = Rect::from_min_size(snap(ui.ctx(), area.center() - size / 2.0), size);
                ui.painter().image(texture.id(), rect, uv(), Color32::WHITE);
                rect
            }
            Scaling::Native => {
                let mut rect = Rect::NOTHING;
                egui::ScrollArea::both()
                    .scroll_source(egui::scroll_area::ScrollSource::SCROLL_BAR)
                    .show(ui, |ui| {
                        // Écran distant plus petit que la fenêtre : centré.
                        let size = native.max(ui.available_size());
                        let (area, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        let min = area.center() - native / 2.0;
                        // Pixels de l'écran entiers : image nette.
                        rect = Rect::from_min_size(snap(ui.ctx(), min), native);
                        ui.painter().image(texture.id(), rect, uv(), Color32::WHITE);
                    });
                rect
            }
        };
        let pointer = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| ui.ui_contains_pointer() && rect.contains(*p));
        if let Some(pos) = pointer {
            self.draw_cursor(ui, cursor, pos, rect.width() / texture.size()[0] as f32);
        }
        if !self.settings.view_only && !self.dialog_open() {
            self.pointer_events(ui, rect, texture.size());
        }
    }

    /// Curseur du serveur, dessiné à la place de celui de ce PC au-dessus de l'écran distant, à
    /// l'échelle de l'image (`scale` : points par pixel distant), lissé.
    fn draw_cursor(&mut self, ui: &egui::Ui, cursor: Option<&crate::decode::Cursor>, pos: Pos2, scale: f32) {
        let ctx = ui.ctx();
        let Some(cursor) = cursor else {
            // Serveur sans curseur séparé : il est dessiné dans l'image.
            ctx.set_cursor_icon(CursorIcon::Crosshair);
            return;
        };
        // Curseur caché par le serveur, ou dessiné ici.
        ctx.set_cursor_icon(CursorIcon::None);
        if cursor.width == 0 || cursor.height == 0 {
            return;
        }
        if self.cursor.as_ref().is_none_or(|(c, _)| c != cursor) {
            let image = ColorImage::from_rgba_unmultiplied([cursor.width, cursor.height], &cursor.rgba);
            // Lissé, avec des versions réduites pour une image très réduite.
            let options = TextureOptions {
                mipmap_mode: Some(egui::TextureFilter::Linear),
                ..TextureOptions::LINEAR
            };
            let texture = ctx.load_texture("curseur", image, options);
            self.cursor = Some((cursor.clone(), texture));
        }
        let (_, texture) = self.cursor.as_ref().expect("curseur");
        let hotspot = Vec2::new(cursor.hotspot[0] as f32, cursor.hotspot[1] as f32) * scale;
        let size = Vec2::new(cursor.width as f32, cursor.height as f32) * scale;
        let rect = Rect::from_min_size(snap(ui.ctx(), pos - hotspot), size);
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::unique("curseur distant"),
        ))
        .image(texture.id(), rect, uv(), Color32::WHITE);
    }

    fn pointer_events(&mut self, ui: &egui::Ui, rect: Rect, size: [usize; 2]) {
        let layer = ui.layer_id();
        let ctx = ui.ctx().clone();
        let on_screen = |pos: Pos2| rect.contains(pos) && ctx.layer_id_at(pos) == Some(layer);
        let to_remote = |pos: Pos2| -> (u16, u16) {
            let rel = (pos - rect.min) / rect.size();
            let x = (rel.x * size[0] as f32).clamp(0.0, size[0] as f32 - 1.0);
            let y = (rel.y * size[1] as f32).clamp(0.0, size[1] as f32 - 1.0);
            (x as u16, y as u16)
        };
        let events = ui.input(|i| i.events.clone());
        for event in events {
            match event {
                Event::PointerMoved(pos) if on_screen(pos) || self.buttons != 0 => {
                    let p = to_remote(pos);
                    if self.pointer != Some(p) {
                        self.pointer = Some(p);
                        self.session.pointer(self.buttons, p.0, p.1);
                    }
                }
                Event::PointerButton {
                    pos, button, pressed, ..
                } => {
                    let mask = session::button_mask(button);
                    if mask == 0 || (pressed && !on_screen(pos)) || (!pressed && self.buttons & mask == 0) {
                        continue;
                    }
                    if pressed {
                        self.buttons |= mask;
                    } else {
                        self.buttons &= !mask;
                    }
                    let p = to_remote(pos);
                    self.pointer = Some(p);
                    self.session.pointer(self.buttons, p.0, p.1);
                }
                Event::MouseWheel { unit, delta, .. } => {
                    let Some(p) = self
                        .pointer
                        .filter(|_| ui.input(|i| i.pointer.hover_pos()).is_some_and(on_screen))
                    else {
                        continue;
                    };
                    self.wheel += match unit {
                        MouseWheelUnit::Point => delta / POINTS_PER_NOTCH,
                        MouseWheelUnit::Line => delta,
                        MouseWheelUnit::Page => delta * 3.0,
                    };
                    // Boutons 4 à 7 : haut, bas, gauche, droite ; un clic par cran.
                    for (axis, positive, negative) in [(1, 8u16, 16u16), (0, 32, 64)] {
                        while self.wheel[axis].abs() >= 1.0 {
                            let button = if self.wheel[axis] > 0.0 { positive } else { negative };
                            self.wheel[axis] -= self.wheel[axis].signum();
                            self.session.pointer(self.buttons | button, p.0, p.1);
                            self.session.pointer(self.buttons, p.0, p.1);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn security_label(ui: &mut egui::Ui, security: Option<SecurityInfo>) {
        let Some(security) = security else { return };
        let (text, color) = if security.encrypted {
            (tr!("Chiffré", "Encrypted"), Color32::from_rgb(0x2e, 0x9e, 0x48))
        } else {
            (tr!("Non chiffré", "Not encrypted"), Color32::from_rgb(0xd9, 0x7a, 0x00))
        };
        ui.label(RichText::new(text).color(color).strong())
            .on_hover_text(security.describe());
    }

    /// Commandes communes à la barre d'outils et au menu F8 ; renvoie leurs éléments, dans l'ordre.
    fn commands(&mut self, ui: &mut egui::Ui, out: &mut Output) -> Vec<egui::Response> {
        let before = self.settings.clone();
        let mut items = Vec::new();
        let fullscreen = if self.fullscreen {
            tr!("Quitter le plein écran", "Exit full screen")
        } else {
            tr!("Plein écran", "Full screen")
        };
        let button = ui.button(fullscreen);
        if button.clicked() {
            self.fullscreen = !self.fullscreen;
            ui.ctx().send_viewport_cmd(ViewportCommand::Fullscreen(self.fullscreen));
            self.settings.fullscreen = self.fullscreen;
            self.menu = false;
        }
        items.push(button);
        items.push(
            ui.selectable_value(&mut self.settings.scaling, Scaling::Fit, tr!("Ajuster", "Fit"))
                .on_hover_text(tr!(
                    "Écran distant ajusté à la fenêtre",
                    "Remote screen scaled to the window"
                )),
        );
        items.push(
            ui.selectable_value(&mut self.settings.scaling, Scaling::Native, "100 %")
                .on_hover_text(tr!(
                    "Un pixel distant par pixel de l'écran",
                    "One remote pixel per screen pixel"
                )),
        );
        let quality = egui::ComboBox::from_id_salt("qualite")
            .selected_text(quality_label(self.settings.quality))
            .show_ui(ui, |ui| {
                for q in [Quality::Auto, Quality::High, Quality::Medium, Quality::Low] {
                    ui.selectable_value(&mut self.settings.quality, q, quality_label(q));
                }
            })
            .response
            .on_hover_text(tr!("Qualité de l'image", "Picture quality"));
        items.push(quality);
        let button = ui.add_enabled(
            !self.settings.view_only,
            egui::Button::new(tr!("Ctrl+Alt+Suppr", "Ctrl+Alt+Del")),
        );
        if button.clicked() {
            self.session.ctrl_alt_del();
            self.menu = false;
        }
        items.push(button);
        items.push(ui.toggle_value(&mut self.settings.view_only, tr!("Lecture seule", "View only")));
        let button = ui.button(tr!("Actualiser", "Refresh"));
        if button.clicked() {
            self.session.refresh();
            self.menu = false;
        }
        items.push(button);
        let button = ui.button(tr!("Informations", "Session info"));
        if button.clicked() {
            self.info = true;
            self.menu = false;
        }
        items.push(button);
        let button = ui.button(tr!("Propriétés", "Properties"));
        if button.clicked() {
            self.properties = Some(self.settings.clone());
            self.menu = false;
        }
        items.push(button);
        let button = ui.button(tr!("Déconnecter", "Disconnect"));
        out.close |= button.clicked();
        items.push(button);
        self.apply(&before, out);
        items
    }

    /// Applique à la session les réglages changés depuis `before`.
    fn apply(&mut self, before: &Settings, out: &mut Output) {
        if *before == self.settings {
            return;
        }
        if before.scaling != self.settings.scaling {
            self.texture = None;
        }
        if (before.quality, before.clipboard) != (self.settings.quality, self.settings.clipboard) {
            self.session.set_options(self.settings.quality, self.settings.clipboard);
        }
        out.settings_changed = true;
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, security: Option<SecurityInfo>, out: &mut Output) {
        ui.horizontal_wrapped(|ui| {
            Self::security_label(ui, security);
            ui.separator();
            self.commands(ui, out);
        });
    }

    /// En plein écran : la barre apparaît en approchant la souris du haut de l'écran.
    fn fullscreen_bar(&mut self, ui: &mut egui::Ui, security: Option<SecurityInfo>, out: &mut Output) {
        let ctx = ui.ctx().clone();
        let id = egui::Id::unique("barre plein écran");
        let near_top = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y < 4.0);
        let shown = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
        if !(near_top || shown) {
            return;
        }
        let response = egui::Area::new(id)
            .anchor(egui::Align2::CENTER_TOP, Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        Self::security_label(ui, security);
                        ui.separator();
                        self.commands(ui, out);
                    });
                });
            })
            .response;
        let keep = response
            .rect
            .expand(16.0)
            .contains(ctx.input(|i| i.pointer.hover_pos()).unwrap_or(Pos2::new(-1.0, -1.0)));
        ctx.data_mut(|d| d.insert_temp(id, near_top || keep));
    }

    /// `step` : déplacement demandé au clavier (`menu_keys`).
    fn f8_menu(&mut self, ui: &mut egui::Ui, out: &mut Output, step: isize) {
        let ctx = ui.ctx().clone();
        let mut open = true;
        egui::Window::new(tr!("Session", "Session"))
            .id(egui::Id::unique("menu F8"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(&ctx, |ui| {
                ui.vertical_centered_justified(|ui| {
                    let mut items = self.commands(ui, out);
                    ui.separator();
                    let button = ui.add_enabled(
                        !self.settings.view_only,
                        egui::Button::new(tr!("Envoyer F8", "Send F8")),
                    );
                    if button.clicked() {
                        self.session.key(true, keys::F1 + 7);
                        self.session.key(false, keys::F1 + 7);
                        self.menu = false;
                    }
                    items.push(button);
                    let toolbar = ui.checkbox(&mut self.settings.toolbar, tr!("Barre d'outils", "Toolbar"));
                    out.settings_changed |= toolbar.changed();
                    items.push(toolbar);
                    menu_navigation(ui, &items, step);
                });
            });
        if !open || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.menu = false;
        }
    }

    fn info_window(&mut self, ui: &mut egui::Ui, security: Option<SecurityInfo>) {
        let ctx = ui.ctx().clone();
        let (name, size, info) = {
            let st = lock(&self.session.state);
            (st.name.clone(), [st.fb.width, st.fb.height], st.info.clone())
        };
        // Débit mesuré sur au moins une seconde.
        let now = ctx.input(|i| i.time);
        let (since, bytes, _) = self.rate;
        if now - since >= 1.0 {
            let rate = if since > 0.0 {
                info.received.saturating_sub(bytes) as f64 / (now - since)
            } else {
                0.0
            };
            self.rate = (now, info.received, rate);
        }
        ctx.request_repaint_after(Duration::from_secs(1));
        let rate = self.rate.2;
        let (host, port) = match crate::rfb::parse_address(&self.address) {
            Some(a) => (a.host, a.port.to_string()),
            None => (self.address.clone(), String::new()),
        };
        let protocol = if info.server_protocol == info.protocol {
            format!("RFB {}", info.protocol)
        } else {
            let server = &info.server_protocol;
            let used = &info.protocol;
            tr!("RFB {used} (serveur : {server})", "RFB {used} (server: {server})")
        };
        let rows = [
            (tr!("Serveur", "Server"), host),
            ("Port".into(), port),
            (tr!("Protocole", "Protocol"), protocol),
            ("Transport".into(), "TCP".into()),
            (tr!("Bureau", "Desktop"), name),
            (
                tr!("Taille de l'écran", "Screen size"),
                format!("{} × {}", size[0], size[1]),
            ),
            (
                tr!("Sécurité", "Security"),
                security.map(|s| s.describe()).unwrap_or_default(),
            ),
            (
                tr!("Couleurs", "Colours"),
                info.format
                    .map(|f| {
                        let bits = f.bits_per_pixel;
                        tr!("{bits} bits par pixel", "{bits} bits per pixel")
                    })
                    .unwrap_or_default(),
            ),
            (
                tr!("Encodages reçus", "Encodings received"),
                info.encodings
                    .iter()
                    .map(|&e| encoding_name(e))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            (tr!("Mises à jour", "Updates"), info.updates.to_string()),
            (
                tr!("Données reçues", "Data received"),
                format!("{} ({}/s)", human_size(info.received as f64), human_size(rate)),
            ),
            (
                tr!("Boutons « précédent » / « suivant »", "\"Back\" / \"forward\" buttons"),
                if info.extended_mouse {
                    tr!("les deux", "both")
                } else {
                    tr!(
                        "« précédent » seul (serveur sans boutons étendus)",
                        "\"back\" only (no extended buttons on the server)"
                    )
                },
            ),
            (
                tr!("Presse-papiers", "Clipboard"),
                if info.extended_clipboard {
                    tr!("étendu (texte Unicode)", "extended (Unicode text)")
                } else {
                    tr!(
                        "classique (Latin-1 : sans €, Œ… ni autres écritures)",
                        "classic (Latin-1: no €, Œ… nor other scripts)"
                    )
                },
            ),
            #[cfg(windows)]
            (
                tr!("Clavier", "Keyboard"),
                if crate::hook::installed() {
                    tr!(
                        "touches spéciales prises à Windows et envoyées au serveur",
                        "special keys taken from Windows and sent to the server"
                    )
                } else {
                    tr!(
                        "sans les touches spéciales (crochet Windows indisponible)",
                        "no special keys (Windows hook unavailable)"
                    )
                },
            ),
            #[cfg(target_os = "linux")]
            (tr!("Clavier", "Keyboard"), {
                use crate::grab::Shortcuts;
                match self.shortcuts {
                    Shortcuts::Unsupported => tr!(
                        "raccourcis du bureau laissés au bureau (non pris en charge par celui-ci)",
                        "desktop shortcuts left to the desktop (not supported by it)"
                    ),
                    Shortcuts::NotRequested => tr!(
                        "raccourcis du bureau laissés au bureau (touches spéciales désactivées, lecture seule, menu ouvert ou fenêtre sans le focus)",
                        "desktop shortcuts left to the desktop (special keys off, view only, menu open or window not focused)"
                    ),
                    Shortcuts::Pending => tr!(
                        "raccourcis du bureau demandés, pas encore accordés par celui-ci (ou refusés)",
                        "desktop shortcuts requested, not granted by it yet (or denied)"
                    ),
                    Shortcuts::Taken => tr!(
                        "raccourcis du bureau pris et envoyés au serveur",
                        "desktop shortcuts taken and sent to the server"
                    ),
                    Shortcuts::Returned => tr!(
                        "raccourcis du bureau accordés, mais repris par celui-ci",
                        "desktop shortcuts granted, but taken back by it"
                    ),
                }
            }),
        ];
        let mut open = true;
        egui::Window::new(tr!("Informations de session", "Session information"))
            .id(egui::Id::unique("informations"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(&ctx, |ui| {
                egui::Grid::new("infos")
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for (label, value) in rows {
                            ui.label(label);
                            ui.label(value);
                            ui.end_row();
                        }
                    });
            });
        if !open || ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.info = false;
        }
    }

    /// Réglages de la connexion, appliqués à la session (OK) et gardés dans le carnet.
    fn properties_window(&mut self, ctx: &egui::Context, out: &mut Output) {
        let Some(settings) = &mut self.properties else { return };
        let mut close = false;
        let mut save = false;
        let label = &self.label;
        let title = tr!("Propriétés — {label}", "Properties — {label}");
        let keys = crate::app::window(ctx, &format!("propriétés {}", self.id), title, [520.0, 520.0], |ui| {
            crate::app::settings_ui(ui, settings, true);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                save = ui.button("OK").clicked();
                close |= ui.button(tr!("Annuler", "Cancel")).clicked();
            });
        });
        save |= keys.enter;
        close |= keys.close;
        if save {
            let before = std::mem::replace(&mut self.settings, settings.clone());
            self.properties = None;
            self.apply(&before, out);
        } else if close {
            self.properties = None;
        }
    }

    /// `first_frame` : la question vient d'apparaître, le focus va au premier champ à remplir.
    fn prompt(&mut self, ui: &mut egui::Ui, prompt: &Prompt, first_frame: bool) {
        let mut answer = None;
        overlay(ui, |ui| match prompt {
            Prompt::Credentials { username } => {
                ui.label(RichText::new(tr!("Authentification", "Authentication")).strong());
                ui.add_space(6.0);
                let mut submit = false;
                egui::Grid::new("identifiants").num_columns(2).show(ui, |ui| {
                    if *username {
                        ui.label(tr!("Nom d'utilisateur", "Username"));
                        let r = ui.text_edit_singleline(&mut self.user);
                        if first_frame && self.user.is_empty() {
                            r.request_focus();
                        }
                        submit |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        ui.end_row();
                    }
                    ui.label(tr!("Mot de passe", "Password"));
                    let r = ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                    if first_frame && (!*username || !self.user.is_empty()) {
                        r.request_focus();
                    }
                    submit |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.end_row();
                });
                ui.checkbox(
                    &mut self.remember,
                    tr!("Mémoriser le mot de passe", "Remember password"),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() || submit {
                        answer = Some(Answer::Credentials {
                            user: self.user.clone(),
                            password: self.password.clone(),
                            remember: self.remember,
                        });
                    }
                    if ui.button(tr!("Annuler", "Cancel")).clicked() {
                        answer = Some(Answer::Cancel);
                    }
                });
            }
            Prompt::Fingerprint { fingerprint, known } => {
                if known.is_empty() {
                    ui.label(RichText::new(tr!("Nouveau serveur", "New server")).strong());
                    ui.label(tr!(
                        "Première connexion chiffrée à ce serveur. Vérifiez que cette signature est bien\n\
                         celle affichée par le serveur VNC (fenêtre d'informations du serveur) :",
                        "First encrypted connection to this server. Check that this signature matches\n\
                         the one shown by the VNC server (server information window):"
                    ));
                } else {
                    ui.label(
                        RichText::new(tr!(
                            "L'identité du serveur a changé !",
                            "The server identity has changed!"
                        ))
                        .strong()
                        .color(Color32::from_rgb(0xd0, 0x30, 0x30)),
                    );
                    ui.label(tr!(
                        "Le serveur a été réinstallé, ou quelqu'un se fait passer pour lui.\nSignature acceptée avant : {known}",
                        "The server was reinstalled, or someone is impersonating it.\nPreviously accepted signature: {known}"
                    ));
                }
                ui.label(RichText::new(fingerprint).monospace().size(16.0));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(tr!("Continuer", "Continue")).clicked() {
                        answer = Some(Answer::Accept);
                    }
                    if ui.button(tr!("Annuler", "Cancel")).clicked() {
                        answer = Some(Answer::Cancel);
                    }
                });
            }
        });
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            answer = Some(Answer::Cancel);
        }
        if let Some(answer) = answer {
            self.session.answer(answer);
        }
    }
}

/// Navigation au clavier d'un menu : flèches haut et bas (ou `Tab` et `Maj+Tab`) d'un élément à
/// l'autre, en boucle, sans en sortir ; elle part du premier élément. Elle remplace celle de la
/// fenêtre, qui va vers l'élément le plus proche à l'écran (ou suivant pour `Tab`), même hors du
/// menu. Les touches sont prises en début d'image, avant que les éléments ne les voient : renvoie
/// le déplacement demandé (-1, 0 ou 1). Une liste ouverte (qualité) garde les siennes.
fn menu_keys(ui: &egui::Ui) -> isize {
    if egui::Popup::is_any_open(ui.ctx()) {
        return 0;
    }
    ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
    ui.input_mut(|i| {
        if i.consume_key(Modifiers::SHIFT, Key::Tab) || i.consume_key(Modifiers::NONE, Key::ArrowUp) {
            -1
        } else if i.consume_key(Modifiers::NONE, Key::Tab) || i.consume_key(Modifiers::NONE, Key::ArrowDown) {
            1
        } else {
            0
        }
    })
}

/// Applique le déplacement de `menu_keys` aux éléments du menu (les éléments grisés sont sautés).
fn menu_navigation(ui: &egui::Ui, items: &[egui::Response], step: isize) {
    let items: Vec<_> = items.iter().filter(|r| r.enabled()).collect();
    if items.is_empty() {
        return;
    }
    let focused = ui.memory(|m| m.focused());
    let next = match focused.and_then(|id| items.iter().position(|r| r.id == id)) {
        Some(i) => (i as isize + step).rem_euclid(items.len() as isize) as usize,
        None => 0,
    };
    if focused != Some(items[next].id) {
        items[next].request_focus();
    }
}

/// Position arrondie au pixel de l'écran (image nette, sans demi-pixels).
fn snap(ctx: &egui::Context, pos: Pos2) -> Pos2 {
    let ppp = ctx.pixels_per_point();
    (pos.to_vec2() * ppp).round().to_pos2() / ppp
}

fn uv() -> Rect {
    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))
}

/// Cadre centré au-dessus de l'écran.
fn overlay(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    let ctx = ui.ctx().clone();
    egui::Area::new(egui::Id::unique("message"))
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .order(egui::Order::Foreground)
        .show(&ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(16.0).show(ui, content);
        });
}

pub fn quality_label(quality: Quality) -> String {
    match quality {
        Quality::Auto => tr!("Automatique", "Automatic"),
        Quality::High => tr!("Haute (sans perte)", "High (lossless)"),
        Quality::Medium => tr!("Moyenne (65 536 couleurs)", "Medium (65,536 colours)"),
        Quality::Low => tr!("Basse (256 couleurs)", "Low (256 colours)"),
    }
}

fn beep(ctx: &egui::Context, focused: bool) {
    #[cfg(windows)]
    // SAFETY : simple son du système.
    unsafe {
        let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(windows::Win32::UI::WindowsAndMessaging::MB_OK);
    }
    if !focused {
        ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Informational,
        ));
    }
}

/// Clavier par les évènements de la fenêtre : caractères tapés (AltGr et touches mortes compris),
/// touches spéciales, raccourcis avec Ctrl ou Alt. Sous Windows, le crochet clavier y ajoute ce que
/// Windows garde pour lui ou que la fenêtre ignore ; sous Linux, le bureau les laisse à la fenêtre
/// quand elle les lui demande (`grab`).
#[derive(Default)]
struct Keyboard {
    modifiers: Modifiers,
    /// Touches appuyées envoyées au serveur.
    pressed: Vec<(Key, u32)>,
}

impl Keyboard {
    fn release(&mut self, session: &Session) {
        for (_, keysym) in self.pressed.drain(..) {
            session.key(false, keysym);
        }
        self.sync(session, Modifiers::NONE);
    }

    fn sync(&mut self, session: &Session, modifiers: Modifiers) {
        for (now, before, keysym) in [
            (modifiers.shift, self.modifiers.shift, keys::SHIFT_L),
            (modifiers.ctrl, self.modifiers.ctrl, keys::CONTROL_L),
            (modifiers.alt, self.modifiers.alt, keys::ALT_L),
            (modifiers.mac_cmd, self.modifiers.mac_cmd, keys::SUPER_L),
        ] {
            if now != before {
                session.key(now, keysym);
            }
        }
        self.modifiers = modifiers;
    }

    /// `settings` : touches spéciales, de volume et multimédia envoyées ou laissées à ce PC.
    fn events(&mut self, ui: &egui::Ui, session: &Session, settings: &Settings) {
        let (events, modifiers) = ui.input(|i| (i.events.clone(), i.modifiers));
        for event in events {
            match event {
                Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } => {
                    self.sync(session, modifiers);
                    let keysym = keys::special(key)
                        .or_else(|| {
                            (modifiers.ctrl || modifiers.alt)
                                .then(|| keys::printable(key, modifiers.shift))
                                .flatten()
                        })
                        .filter(|&k| keys::group(k).is_none_or(|g| settings.sends(g)));
                    match (pressed, keysym) {
                        (true, Some(keysym)) => {
                            session.key(true, keysym);
                            if !self.pressed.iter().any(|(k, _)| *k == key) {
                                self.pressed.push((key, keysym));
                            }
                        }
                        (false, _) => {
                            if let Some(i) = self.pressed.iter().position(|(k, _)| *k == key) {
                                session.key(false, self.pressed.remove(i).1);
                            }
                        }
                        _ => {}
                    }
                }
                // Avec Ctrl ou Alt, la touche est déjà partie (Event::Key).
                Event::Text(text) if !self.modifiers.ctrl && !self.modifiers.alt => {
                    for c in text.chars().filter(|c| !c.is_control()) {
                        session.key(true, keys::char_keysym(c));
                        session.key(false, keys::char_keysym(c));
                    }
                }
                // Raccourcis du presse-papiers, transformés par la fenêtre : Ctrl est déjà appuyé (sous Windows,
                // le crochet prend avant elle ceux faits avec Inser ou Suppr).
                Event::Copy | Event::Cut | Event::Paste(_) => {
                    let c = match event {
                        Event::Copy => 'c',
                        Event::Cut => 'x',
                        _ => 'v',
                    };
                    session.key(true, keys::char_keysym(c));
                    session.key(false, keys::char_keysym(c));
                }
                _ => {}
            }
        }
        self.sync(session, modifiers);
    }
}

fn encoding_name(encoding: i32) -> String {
    use crate::decode::*;
    match encoding {
        RAW => "Raw".into(),
        COPY_RECT => "CopyRect".into(),
        RRE => "RRE".into(),
        HEXTILE => "Hextile".into(),
        ZLIB => "Zlib".into(),
        TIGHT => "Tight".into(),
        ZRLE => "ZRLE".into(),
        other => other.to_string(),
    }
}

/// Taille en octets, en Kio / Mio / Gio.
fn human_size(bytes: f64) -> String {
    let units = if crate::i18n::french() {
        ["o", "Kio", "Mio", "Gio"]
    } else {
        ["B", "KiB", "MiB", "GiB"]
    };
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let text = if unit == 0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    };
    let text = if crate::i18n::french() {
        text.replace('.', ",")
    } else {
        text
    };
    format!("{text} {}", units[unit])
}

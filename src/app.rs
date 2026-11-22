//! Fenêtre principale : carnet d'adresses (recherche, connexion, propriétés), options, et une
//! fenêtre par session ouverte.

use eframe::egui::{
    self, Align, Color32, Key, Layout, Modifiers, RichText, ThemePreference, ViewportCommand, ViewportId,
};

use crate::config::{Config, Connection, Display, Encryption, Language, Quality, Scaling, Settings, Theme};
use crate::session::{Params, Session, Status, lock};
use crate::viewer::{View, quality_label};
use crate::{rfb, secret};

pub const APP_NAME: &str = "vnc-viewer";
/// Place de la pastille verte d'une entrée ouverte, dans le carnet.
const DOT_SPACE: f32 = 14.0;

/// Entrée du carnet en cours de modification.
struct Editor {
    /// `None` : nouvelle entrée.
    id: Option<u64>,
    conn: Connection,
    /// Mot de passe en clair pendant la modification.
    password: String,
}

struct OptionsDialog {
    display: Display,
    defaults: Settings,
}

pub struct App {
    config: Config,
    error: Option<String>,
    search: String,
    selected: Option<u64>,
    editor: Option<Editor>,
    /// Entrée dont la suppression attend confirmation.
    delete: Option<u64>,
    options: Option<OptionsDialog>,
    about: bool,
    /// Fermeture de la fenêtre principale demandée alors que des sessions sont ouvertes.
    quit: bool,
    views: Vec<View>,
    next_viewport: u64,
    clipboard: Option<arboard::Clipboard>,
    #[cfg(target_os = "linux")]
    grab: crate::grab::Grab,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, (config, error): (Config, Option<String>)) -> Self {
        cc.egui_ctx.set_theme(theme(config.display.theme));
        // Ctrl+Plus et Ctrl+Moins vont au serveur, pas au zoom de l'interface.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        #[cfg(windows)]
        crate::hook::install();
        App {
            config,
            error,
            search: String::new(),
            selected: None,
            editor: None,
            delete: None,
            options: None,
            about: false,
            quit: false,
            views: Vec::new(),
            next_viewport: 0,
            clipboard: arboard::Clipboard::new().ok(),
            #[cfg(target_os = "linux")]
            grab: crate::grab::Grab::new(raw_window_handle::HasDisplayHandle::display_handle(cc)),
        }
    }

    fn save(&mut self) {
        if let Err(e) = self.config.save() {
            self.error = Some(tr!("Enregistrement impossible : {e}", "Could not save: {e}"));
        }
    }

    /// Ouvre une session (ou affiche celle déjà ouverte pour cette entrée).
    fn connect(&mut self, ctx: &egui::Context, id: u64) {
        if let Some(view) = self.views.iter().find(|v| v.id == id) {
            ctx.send_viewport_cmd_to(view.viewport, ViewportCommand::Focus);
            return;
        }
        let Some(conn) = self.config.connection(id).cloned() else {
            return;
        };
        let Some(session) = self.start(ctx, &conn, false) else {
            return;
        };
        let viewport = ViewportId::from_hash_of(("session", self.next_viewport));
        self.next_viewport += 1;
        self.views.push(View::new(&conn, session, viewport));
    }

    /// Adresse tapée : l'entrée du carnet qui y mène, sinon une nouvelle.
    fn connect_address(&mut self, ctx: &egui::Context, address: &str) {
        if rfb::parse_address(address).is_none() {
            self.error = Some(tr!("Adresse invalide : {address}", "Invalid address: {address}"));
            return;
        }
        let id = match self.config.find_address(address) {
            Some(id) => id,
            None => {
                let id = self.config.add("", address);
                self.save();
                id
            }
        };
        self.selected = Some(id);
        self.connect(ctx, id);
    }

    fn start(&mut self, ctx: &egui::Context, conn: &Connection, ask_password: bool) -> Option<Session> {
        let Some(address) = rfb::parse_address(&conn.address) else {
            let address = &conn.address;
            self.error = Some(tr!("Adresse invalide : {address}", "Invalid address: {address}"));
            return None;
        };
        let params = Params {
            address,
            settings: conn.settings.clone(),
            username: conn.username.clone(),
            password: if ask_password {
                None
            } else {
                secret::unprotect(&conn.password)
            },
            fingerprint: conn.fingerprint.clone(),
        };
        let ctx = ctx.clone();
        Some(Session::start(params, move || ctx.request_repaint()))
    }

    fn views(&mut self, ctx: &egui::Context) {
        let mut closed = Vec::new();
        for i in 0..self.views.len() {
            let output = self.views[i].show(ctx, &mut self.clipboard);
            let (id, settings) = (self.views[i].id, self.views[i].settings.clone());
            let mut changed = false;
            if let Some(conn) = self.config.connection_mut(id) {
                if let Some((user, password)) = output.credentials {
                    if !user.is_empty() {
                        conn.username = user;
                    }
                    conn.password = secret::protect(&password);
                    changed = true;
                }
                if let Some(fingerprint) = output.fingerprint {
                    conn.fingerprint = fingerprint;
                    changed = true;
                }
                if output.settings_changed {
                    conn.settings = settings;
                    changed = true;
                }
            }
            if changed {
                self.save();
            }
            if output.close {
                closed.push(i);
            } else if output.reconnect {
                // Mot de passe refusé : le redemander.
                let ask = matches!(&lock(&self.views[i].session.state).status, Status::Closed(c) if c.auth_failed);
                if let Some(conn) = self.config.connection(id).cloned()
                    && let Some(session) = self.start(ctx, &conn, ask)
                {
                    self.views[i].replace_session(&conn, session);
                }
            }
        }
        for i in closed.into_iter().rev() {
            self.views.remove(i);
        }
        #[cfg(windows)]
        crate::hook::set_target(self.views.iter().find(|v| v.focused() && v.capturing()).map(|v| {
            crate::hook::Target {
                id: v.id,
                keys: v.session.keys(),
                settings: v.settings.clone(),
            }
        }));
        #[cfg(target_os = "linux")]
        {
            let sending: Vec<u64> = (self.views.iter())
                .filter(|v| v.capturing() && v.settings.special_keys)
                .map(|v| v.id)
                .collect();
            let focused = self.views.iter().find(|v| v.focused() && sending.contains(&v.id));
            if let Some(delay) = self.grab.update(focused.map(|v| v.id), &sending) {
                ctx.request_repaint_after(delay);
            }
            for view in &mut self.views {
                view.shortcuts = self.grab.status(view.id);
            }
        }
    }

    /// Entrées affichées : celles qui correspondent à la recherche, par ordre alphabétique.
    fn filtered(&self) -> Vec<&Connection> {
        let search = self.search.trim().to_lowercase();
        let mut list: Vec<&Connection> = self
            .config
            .connections
            .iter()
            .filter(|c| {
                search.is_empty()
                    || c.label().to_lowercase().contains(&search)
                    || c.address.to_lowercase().contains(&search)
            })
            .collect();
        list.sort_by_key(|c| c.label().to_lowercase());
        list
    }

    fn edit(&mut self, id: Option<u64>) {
        let conn = match id.and_then(|id| self.config.connection(id)) {
            Some(conn) => conn.clone(),
            None => Connection {
                address: self.search.trim().to_owned(),
                settings: self.config.defaults.clone(),
                ..Default::default()
            },
        };
        let password = secret::unprotect(&conn.password).unwrap_or_default();
        self.editor = Some(Editor { id, conn, password });
    }

    fn duplicate(&mut self, id: u64) {
        let Some(conn) = self.config.connection(id).cloned() else {
            return;
        };
        let label = conn.label();
        let new = self
            .config
            .add(&tr!("{label} (copie)", "{label} (copy)"), &conn.address);
        if let Some(c) = self.config.connection_mut(new) {
            c.username = conn.username;
            c.password = conn.password;
            c.fingerprint = conn.fingerprint;
            c.settings = conn.settings;
        }
        self.selected = Some(new);
        self.save();
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog_open() {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            self.edit(None);
        }
        // Suppr et Entrée appartiennent au champ de saisie qui a le focus.
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete)) {
            self.delete = self.selected;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
            && let Some(id) = self.selected
        {
            self.connect(ctx, id);
        }
    }

    fn dialog_open(&self) -> bool {
        self.editor.is_some()
            || self.delete.is_some()
            || self.options.is_some()
            || self.about
            || self.quit
            || self.error.is_some()
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(tr!("Fichier", "File"), |ui| {
                if ui
                    .add(egui::Button::new(tr!("Nouvelle connexion…", "New connection…")).shortcut_text("Ctrl+N"))
                    .clicked()
                {
                    self.edit(None);
                }
                if ui.button(tr!("Options…", "Preferences…")).clicked() {
                    self.options = Some(OptionsDialog {
                        display: self.config.display.clone(),
                        defaults: self.config.defaults.clone(),
                    });
                }
                ui.separator();
                if ui.button(tr!("Quitter", "Exit")).clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
            });
            ui.menu_button(tr!("Aide", "Help"), |ui| {
                if ui.button(tr!("À propos…", "About…")).clicked() {
                    self.about = true;
                }
            });
        });
    }

    fn search_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let connect = tr!("Se connecter", "Connect");
            let button_width = ui.spacing().interact_size.x.max(100.0);
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(tr!(
                        "Adresse du serveur VNC (hôte, hôte:écran, hôte::port) ou recherche",
                        "VNC server address (host, host:display, host::port) or search"
                    ))
                    .desired_width(ui.available_width() - button_width - 8.0),
            );
            let entered = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let clicked = ui
                .add_enabled(!self.search.trim().is_empty(), egui::Button::new(connect))
                .clicked();
            if (entered || clicked) && !self.search.trim().is_empty() {
                let ctx = ui.ctx().clone();
                let found: Vec<u64> = self.filtered().iter().map(|c| c.id).collect();
                match found.as_slice() {
                    [id] => self.connect(&ctx, *id),
                    _ => {
                        let address = self.search.trim().to_owned();
                        self.connect_address(&ctx, &address);
                    }
                }
            }
        });
        ui.add_space(6.0);
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        let rows: Vec<(u64, String, String)> = self
            .filtered()
            .iter()
            .map(|c| (c.id, c.label().to_owned(), c.address.clone()))
            .collect();
        if rows.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(if self.config.connections.is_empty() {
                    tr!(
                        "Carnet d'adresses vide : tapez l'adresse d'un serveur VNC ci-dessus.",
                        "Empty address book: type a VNC server address above."
                    )
                } else {
                    tr!("Aucune connexion trouvée.", "No connection found.")
                });
            });
            return;
        }
        let ctx = ui.ctx().clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                for (id, label, address) in rows {
                    let open = self.views.iter().any(|v| v.id == id);
                    let mut text = egui::text::LayoutJob::default();
                    let style = ui.style();
                    let strong = egui::TextFormat {
                        color: style.visuals.strong_text_color(),
                        font_id: egui::TextStyle::Body.resolve(style),
                        ..Default::default()
                    };
                    let weak = egui::TextFormat {
                        color: style.visuals.weak_text_color(),
                        ..strong.clone()
                    };
                    // Place de la pastille des sessions ouvertes, dessinée ensuite.
                    text.append(&label, if open { DOT_SPACE } else { 0.0 }, strong);
                    if label != address.trim() {
                        text.append(&address, 16.0, weak);
                    }
                    let response = ui.selectable_label(self.selected == Some(id), text);
                    if open {
                        let center = response.rect.left_center()
                            + egui::vec2(ui.spacing().button_padding.x + DOT_SPACE / 2.0 - 2.0, 0.0);
                        ui.painter()
                            .circle_filled(center, 4.0, Color32::from_rgb(0x2e, 0x9e, 0x48));
                    }
                    if response.clicked() {
                        self.selected = Some(id);
                    }
                    if response.double_clicked() {
                        self.connect(&ctx, id);
                    }
                    response.context_menu(|ui| {
                        self.selected = Some(id);
                        if ui.button(tr!("Se connecter", "Connect")).clicked() {
                            self.connect(&ctx, id);
                        }
                        if ui.button(tr!("Propriétés…", "Properties…")).clicked() {
                            self.edit(Some(id));
                        }
                        if ui.button(tr!("Dupliquer", "Duplicate")).clicked() {
                            self.duplicate(id);
                        }
                        if ui.button(tr!("Supprimer", "Delete")).clicked() {
                            self.delete = Some(id);
                        }
                    });
                }
            });
        });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button(tr!("Nouvelle…", "New…")).clicked() {
                self.edit(None);
            }
            let selected = self.selected.filter(|id| self.config.connection(*id).is_some());
            ui.add_enabled_ui(selected.is_some(), |ui| {
                if ui.button(tr!("Propriétés…", "Properties…")).clicked() {
                    self.edit(selected);
                }
                if ui.button(tr!("Supprimer", "Delete")).clicked() {
                    self.delete = selected;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(tr!("Se connecter", "Connect")).clicked()
                        && let Some(id) = selected
                    {
                        let ctx = ui.ctx().clone();
                        self.connect(&ctx, id);
                    }
                });
            });
        });
        ui.add_space(4.0);
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let escape = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        self.editor_dialog(ctx);
        if let Some(id) = self.delete {
            let label = self
                .config
                .connection(id)
                .map(|c| c.label().to_owned())
                .unwrap_or_default();
            let mut close = escape;
            dialog(ctx, tr!("Supprimer", "Delete"), |ui| {
                ui.label(tr!(
                    "Supprimer « {label} » du carnet d'adresses ?",
                    "Delete \"{label}\" from the address book?"
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr!("Supprimer", "Delete")).clicked() {
                        self.config.connections.retain(|c| c.id != id);
                        self.selected = None;
                        close = true;
                        self.save();
                    }
                    close |= ui.button(tr!("Annuler", "Cancel")).clicked();
                });
            });
            if close {
                self.delete = None;
            }
        }
        self.options_dialog(ctx);
        if self.about {
            let version = env!("CARGO_PKG_VERSION");
            let mut close = escape;
            dialog(ctx, tr!("À propos", "About"), |ui| {
                ui.label(RichText::new(format!("{APP_NAME} {version}")).strong());
                ui.label(tr!(
                    "Visionneuse VNC, sans compte ni connexion à un service en ligne.",
                    "VNC viewer, with no account and no online service."
                ));
                ui.add_space(6.0);
                ui.label(tr!(
                    "Dans une session : F8 ouvre le menu (plein écran, Ctrl+Alt+Suppr, qualité…).",
                    "In a session: F8 opens the menu (full screen, Ctrl+Alt+Del, quality…)."
                ));
                ui.label(tr!("Licence : MIT ou Apache 2.0.", "Licence: MIT or Apache 2.0."));
                ui.add_space(6.0);
                close |= ui.button("OK").clicked();
            });
            if close {
                self.about = false;
            }
        }
        if self.quit {
            let count = self.views.len();
            let mut close = escape;
            dialog(ctx, tr!("Quitter", "Exit"), |ui| {
                ui.label(tr!(
                    "{count} session(s) ouverte(s) seront fermées.",
                    "{count} open session(s) will be closed."
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr!("Quitter", "Exit")).clicked() {
                        self.views.clear();
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                        close = true;
                    }
                    close |= ui.button(tr!("Annuler", "Cancel")).clicked();
                });
            });
            if close {
                self.quit = false;
            }
        }
        if let Some(error) = self.error.clone() {
            let mut close = escape;
            dialog(ctx, tr!("Erreur", "Error"), |ui| {
                ui.label(error);
                ui.add_space(8.0);
                close |= ui.button("OK").clicked();
            });
            if close {
                self.error = None;
            }
        }
    }

    fn editor_dialog(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.editor else { return };
        let title = if editor.id.is_some() {
            tr!("Propriétés", "Properties")
        } else {
            tr!("Nouvelle connexion", "New connection")
        };
        let mut close = false;
        let mut save = false;
        let valid = rfb::parse_address(&editor.conn.address).is_some();
        let keys = window(ctx, "éditeur", title, [520.0, 600.0], |ui| {
            egui::Grid::new("general")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(tr!("Adresse", "Address"));
                    ui.add(
                        egui::TextEdit::singleline(&mut editor.conn.address)
                            .hint_text(tr!(
                                "hôte, hôte:écran ou hôte::port",
                                "host, host:display or host::port"
                            ))
                            .desired_width(280.0),
                    );
                    ui.end_row();
                    ui.label(tr!("Nom", "Name"));
                    ui.add(
                        egui::TextEdit::singleline(&mut editor.conn.name)
                            .hint_text(tr!("(l'adresse)", "(the address)"))
                            .desired_width(280.0),
                    );
                    ui.end_row();
                    ui.label(tr!("Nom d'utilisateur", "Username"));
                    ui.add(
                        egui::TextEdit::singleline(&mut editor.conn.username)
                            .hint_text(tr!("(si le serveur en demande un)", "(if the server asks for one)"))
                            .desired_width(280.0),
                    );
                    ui.end_row();
                    ui.label(tr!("Mot de passe", "Password"));
                    ui.add(
                        egui::TextEdit::singleline(&mut editor.password)
                            .password(true)
                            .hint_text(tr!("(demandé à la connexion)", "(asked when connecting)"))
                            .desired_width(280.0),
                    );
                    ui.end_row();
                    ui.label(tr!("Signature du serveur", "Server signature"));
                    ui.horizontal(|ui| {
                        if editor.conn.fingerprint.is_empty() {
                            ui.weak(tr!("(pas encore acceptée)", "(not yet accepted)"));
                        } else {
                            ui.monospace(&editor.conn.fingerprint);
                            if ui.small_button(tr!("Oublier", "Forget")).clicked() {
                                editor.conn.fingerprint.clear();
                            }
                        }
                    });
                    ui.end_row();
                });
            ui.separator();
            settings_ui(ui, &mut editor.conn.settings, false);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                save = ui.add_enabled(valid, egui::Button::new("OK")).clicked();
                close |= ui.button(tr!("Annuler", "Cancel")).clicked();
            });
        });
        save |= keys.enter && valid;
        close |= keys.close;
        if save {
            let mut conn = editor.conn.clone();
            conn.password = secret::protect(&editor.password);
            let id = match editor.id {
                Some(id) => id,
                None => self.config.add("", &conn.address),
            };
            conn.id = id;
            if let Some(c) = self.config.connection_mut(id) {
                *c = conn;
            }
            self.selected = Some(id);
            self.editor = None;
            self.save();
        } else if close {
            self.editor = None;
        }
    }

    fn options_dialog(&mut self, ctx: &egui::Context) {
        let Some(options) = &mut self.options else { return };
        let mut close = false;
        let mut save = false;
        let keys = window(ctx, "options", tr!("Options", "Preferences"), [520.0, 560.0], |ui| {
            egui::Grid::new("affichage")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(tr!("Langue", "Language"));
                    egui::ComboBox::from_id_salt("langue")
                        .selected_text(language_label(options.display.language))
                        .show_ui(ui, |ui| {
                            for l in [Language::Auto, Language::English, Language::French] {
                                ui.selectable_value(&mut options.display.language, l, language_label(l));
                            }
                        });
                    ui.end_row();
                    ui.label(tr!("Thème", "Theme"));
                    ui.horizontal(|ui| {
                        for (t, label) in [
                            (Theme::System, tr!("Système", "System")),
                            (Theme::Light, tr!("Clair", "Light")),
                            (Theme::Dark, tr!("Sombre", "Dark")),
                        ] {
                            ui.radio_value(&mut options.display.theme, t, label);
                        }
                    });
                    ui.end_row();
                });
            ui.separator();
            ui.label(RichText::new(tr!("Réglages des nouvelles connexions", "Settings for new connections")).strong());
            settings_ui(ui, &mut options.defaults, false);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                save = ui.button("OK").clicked();
                close |= ui.button(tr!("Annuler", "Cancel")).clicked();
            });
        });
        save |= keys.enter;
        close |= keys.close;
        if save {
            self.config.display = options.display.clone();
            self.config.defaults = options.defaults.clone();
            crate::i18n::set(self.config.display.language);
            ctx.set_theme(theme(self.config.display.theme));
            self.options = None;
            self.save();
        } else if close {
            self.options = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && !self.views.is_empty() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.quit = true;
        }
        self.shortcuts(&ctx);
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("recherche").show(ui, |ui| self.search_bar(ui));
        egui::Panel::bottom("boutons").show(ui, |ui| self.bottom_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| self.list(ui));
        self.dialogs(&ctx);
        self.views(&ctx);
    }
}

fn theme(theme: Theme) -> ThemePreference {
    match theme {
        Theme::System => ThemePreference::System,
        Theme::Light => ThemePreference::Light,
        Theme::Dark => ThemePreference::Dark,
    }
}

fn language_label(language: Language) -> String {
    match language {
        Language::Auto => tr!("Automatique (langue du système)", "Automatic (system language)"),
        Language::English => "English (United States)".into(),
        Language::French => "Français (France)".into(),
    }
}

fn encryption_label(encryption: Encryption) -> String {
    match encryption {
        Encryption::AlwaysOn => tr!("Toujours", "Always on"),
        Encryption::PreferOn => tr!("De préférence", "Prefer on"),
        Encryption::PreferOff => tr!("Seulement si imposé", "Prefer off"),
    }
}

/// Touches d'une fenêtre à part : fermeture (croix ou `Échap`) et `Entrée` (validation).
#[derive(Default)]
pub struct WindowKeys {
    pub close: bool,
    pub enter: bool,
}

/// Fenêtre à part, une vraie fenêtre du système : jamais rognée par la fenêtre qui l'ouvre.
pub fn window(
    ctx: &egui::Context,
    id: &str,
    title: String,
    size: [f32; 2],
    mut content: impl FnMut(&mut egui::Ui),
) -> WindowKeys {
    let mut keys = WindowKeys::default();
    let builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(size)
        .with_min_inner_size([320.0, 200.0]);
    ctx.show_viewport_immediate(ViewportId::from_hash_of(id), builder, |ui, _| {
        keys.close = ui.input_mut(|i| i.viewport().close_requested() || i.consume_key(Modifiers::NONE, Key::Escape));
        keys.enter = ui.input(|i| i.key_pressed(Key::Enter));
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, &mut content);
        });
    });
    keys
}

/// Petite fenêtre centrée, au-dessus du reste.
fn dialog(ctx: &egui::Context, title: String, content: impl FnOnce(&mut egui::Ui)) {
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, content);
}

/// Réglages d'une connexion ; `connected` : modifiés pendant la session (le chiffrement et le
/// partage ne changent qu'à la connexion suivante).
pub fn settings_ui(ui: &mut egui::Ui, s: &mut Settings, connected: bool) {
    let next = |text: String| {
        if connected {
            tr!("{text} (à la prochaine connexion)", "{text} (next connection)")
        } else {
            text
        }
    };
    egui::Grid::new(ui.next_auto_id())
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(next(tr!("Chiffrement", "Encryption")));
            egui::ComboBox::from_id_salt("chiffrement")
                .selected_text(encryption_label(s.encryption))
                .show_ui(ui, |ui| {
                    for e in [Encryption::AlwaysOn, Encryption::PreferOn, Encryption::PreferOff] {
                        ui.selectable_value(&mut s.encryption, e, encryption_label(e));
                    }
                });
            ui.end_row();
            ui.label(tr!("Qualité de l'image", "Picture quality"));
            egui::ComboBox::from_id_salt("qualité")
                .selected_text(quality_label(s.quality))
                .show_ui(ui, |ui| {
                    for q in [Quality::Auto, Quality::High, Quality::Medium, Quality::Low] {
                        ui.selectable_value(&mut s.quality, q, quality_label(q));
                    }
                });
            ui.end_row();
            ui.label(tr!("Taille", "Scaling"));
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut s.scaling,
                    Scaling::Fit,
                    tr!("Ajustée à la fenêtre", "Fit to window"),
                );
                ui.radio_value(&mut s.scaling, Scaling::Native, "100 %");
            });
            ui.end_row();
        });
    ui.checkbox(
        &mut s.view_only,
        tr!(
            "Lecture seule (ni souris ni clavier)",
            "View only (no mouse, no keyboard)"
        ),
    );
    ui.checkbox(
        &mut s.shared,
        next(tr!(
            "Partager le serveur (sinon les autres visionneuses sont déconnectées)",
            "Share the server (otherwise other viewers are disconnected)"
        )),
    );
    ui.checkbox(
        &mut s.special_keys,
        tr!(
            "Touches spéciales au serveur : Windows, Impr. écran, Alt+Tab, Alt+Échap, Ctrl+Échap",
            "Special keys to the server: Windows, Print Screen, Alt+Tab, Alt+Esc, Ctrl+Esc"
        ),
    );
    ui.checkbox(
        &mut s.volume_keys,
        tr!(
            "Touches monter, baisser et couper le volume au serveur",
            "Volume up, down and mute keys to the server"
        ),
    );
    ui.checkbox(
        &mut s.media_keys,
        tr!(
            "Touches multimédia (lecture, pause…) au serveur",
            "Media keys (play, pause…) to the server"
        ),
    );
    ui.checkbox(&mut s.clipboard, tr!("Presse-papiers partagé", "Shared clipboard"));
    ui.checkbox(
        &mut s.fullscreen,
        tr!("Plein écran à l'ouverture", "Full screen when opening"),
    );
    ui.checkbox(&mut s.toolbar, tr!("Barre d'outils", "Toolbar"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_ui() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut config = Config::default();
        let id = config.add("Bureau", "bureau:1");
        config.add("", "10.0.0.2::5999");
        let mut app = App::new(&cc, (config, None));
        let mut frame = eframe::Frame::_new_kittest();
        let mut run = |app: &mut App| {
            for _ in 0..2 {
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| eframe::App::ui(app, ui, &mut frame));
                output.textures_delta.clear();
            }
        };
        run(&mut app);
        app.search = "bur".into();
        assert_eq!(app.filtered().len(), 1);
        app.edit(Some(id));
        run(&mut app);
        app.editor = None;
        app.options = Some(OptionsDialog {
            display: Display::default(),
            defaults: Settings::default(),
        });
        run(&mut app);
        app.options = None;
        app.about = true;
        app.error = Some("test".into());
        run(&mut app);
    }
}

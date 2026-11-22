// Pas de console noire derrière la fenêtre sous Windows (en release).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

// En premier : la macro `tr!` sert dans tous les modules.
#[macro_use]
mod i18n;

mod app;
mod config;
mod decode;
#[cfg(target_os = "linux")]
mod grab;
#[cfg(windows)]
mod hook;
mod keys;
mod rfb;
#[cfg(test)]
mod scenario;
mod secret;
mod security;
mod session;
mod viewer;

use eframe::egui;

fn main() {
    let loaded = config::Config::load();
    i18n::set(loaded.0.display.language);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app::APP_NAME)
            .with_app_id(app::APP_NAME)
            .with_inner_size([640.0, 460.0])
            .with_min_inner_size([360.0, 240.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        app::APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, loaded)))),
    );
    if let Err(e) = result {
        // En mode fenêtré il n'y a pas de console : on affiche l'erreur dans une boîte de dialogue.
        rfd::MessageDialog::new()
            .set_title(app::APP_NAME)
            .set_description(tr!("Impossible de démarrer : {e}", "Unable to start: {e}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(1);
    }
}

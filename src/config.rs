//! Configuration (carnet d'adresses, options), enregistrée en TOML.
//!
//! Emplacement : `vnc-viewer.toml` à côté de l'exécutable s'il existe (mode portable),
//! sinon `%APPDATA%\vnc-viewer\config.toml` (Windows) ou `~/.config/vnc-viewer/config.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub display: Display,
    /// Réglages des nouvelles connexions.
    pub defaults: Settings,
    pub connections: Vec<Connection>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    pub language: Language,
    pub theme: Theme,
}

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "en-US")]
    English,
    #[serde(rename = "fr-FR")]
    French,
}

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "light")]
    Light,
    #[serde(rename = "dark")]
    Dark,
}

/// Une entrée du carnet d'adresses.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Connection {
    /// Identifiant stable (sessions ouvertes, sélection).
    pub id: u64,
    /// Nom affiché ; vide : l'adresse.
    pub name: String,
    /// `hôte`, `hôte:écran` (port 5900 + écran) ou `hôte::port`.
    pub address: String,
    /// Nom d'utilisateur (serveurs qui en demandent un).
    pub username: String,
    /// Mot de passe mémorisé, protégé (voir `secret`) ; vide : demandé à la connexion.
    pub password: String,
    /// Empreinte de la clé du serveur acceptée (chiffrement RA2), au format `xx-xx-…`.
    pub fingerprint: String,
    pub settings: Settings,
}

impl Connection {
    pub fn label(&self) -> &str {
        match self.name.trim() {
            "" => self.address.trim(),
            name => name,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub encryption: Encryption,
    pub quality: Quality,
    pub scaling: Scaling,
    /// Écran seulement : ni souris ni clavier envoyés.
    pub view_only: bool,
    /// Laisse les autres sessions ouvertes sur le serveur (sinon elles sont fermées).
    pub shared: bool,
    /// Touche Windows, Impr. écran, Alt+Tab, Alt+Échap, Ctrl+Échap envoyées au serveur plutôt
    /// qu'à ce PC.
    pub special_keys: bool,
    /// Touches monter, baisser et couper le volume envoyées au serveur.
    pub volume_keys: bool,
    /// Touches multimédia (lecture/pause, suivant…) envoyées au serveur.
    pub media_keys: bool,
    /// Presse-papiers partagé dans les deux sens.
    pub clipboard: bool,
    pub fullscreen: bool,
    /// Barre d'outils en haut de la fenêtre (hors plein écran).
    pub toolbar: bool,
}

impl Settings {
    /// Les touches de ce groupe vont au serveur (sinon elles restent à ce PC).
    pub fn sends(&self, group: crate::keys::Group) -> bool {
        use crate::keys::Group;
        match group {
            Group::Special => self.special_keys,
            Group::Volume => self.volume_keys,
            Group::Media => self.media_keys,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            encryption: Encryption::PreferOn,
            quality: Quality::Auto,
            scaling: Scaling::Fit,
            view_only: false,
            shared: true,
            special_keys: true,
            volume_keys: false,
            media_keys: false,
            clipboard: true,
            fullscreen: false,
            toolbar: true,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Debug, Serialize, Deserialize)]
pub enum Encryption {
    /// Refuse un serveur qui ne chiffre pas.
    #[serde(rename = "always")]
    AlwaysOn,
    #[default]
    #[serde(rename = "prefer-on")]
    PreferOn,
    #[serde(rename = "prefer-off")]
    PreferOff,
}

#[derive(Clone, Copy, Default, PartialEq, Debug, Serialize, Deserialize)]
pub enum Quality {
    /// Couleurs exactes, JPEG pour les zones photographiques.
    #[default]
    #[serde(rename = "auto")]
    Auto,
    /// Sans perte.
    #[serde(rename = "high")]
    High,
    /// 65 536 couleurs.
    #[serde(rename = "medium")]
    Medium,
    /// 256 couleurs, pour une liaison lente.
    #[serde(rename = "low")]
    Low,
}

#[derive(Clone, Copy, Default, PartialEq, Debug, Serialize, Deserialize)]
pub enum Scaling {
    /// Écran distant ajusté à la fenêtre, proportions gardées.
    #[default]
    #[serde(rename = "fit")]
    Fit,
    /// Un pixel distant par pixel de l'écran, barres de défilement si besoin.
    #[serde(rename = "native")]
    Native,
}

impl Config {
    /// Configuration enregistrée, ou par défaut, et l'erreur de lecture éventuelle.
    pub fn load() -> (Config, Option<String>) {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(config) => (config, None),
                Err(e) => {
                    let path = path.display();
                    let error = tr!(
                        "Configuration illisible ({path}) : {e}",
                        "Unreadable configuration ({path}): {e}"
                    );
                    (Config::default(), Some(error))
                }
            },
            Err(_) => (Config::default(), None),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                let dir = dir.display();
                tr!("{dir} : {e}", "{dir}: {e}")
            })?;
        }
        std::fs::write(&path, text).map_err(|e| {
            let path = path.display();
            tr!("{path} : {e}", "{path}: {e}")
        })
    }

    pub fn connection(&self, id: u64) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }

    pub fn connection_mut(&mut self, id: u64) -> Option<&mut Connection> {
        self.connections.iter_mut().find(|c| c.id == id)
    }

    /// Nouvelle entrée (réglages par défaut), ajoutée au carnet ; renvoie son identifiant.
    pub fn add(&mut self, name: &str, address: &str) -> u64 {
        let id = self.connections.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        self.connections.push(Connection {
            id,
            name: name.trim().into(),
            address: address.trim().into(),
            settings: self.defaults.clone(),
            ..Default::default()
        });
        id
    }

    /// Entrée dont l'adresse désigne le même serveur (`hôte`, `hôte:0` et `hôte::5900` sont égaux).
    pub fn find_address(&self, address: &str) -> Option<u64> {
        let target = crate::rfb::parse_address(address)?;
        self.connections
            .iter()
            .find(|c| crate::rfb::parse_address(&c.address).is_some_and(|a| a.eq_ignore_case(&target)))
            .map(|c| c.id)
    }
}

pub fn config_path() -> PathBuf {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
    {
        let portable = dir.join("vnc-viewer.toml");
        if portable.is_file() {
            return portable;
        }
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.unwrap_or_default().join("vnc-viewer").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut config = Config::default();
        let id = config.add("Bureau", "pc-bureau:1");
        config.connection_mut(id).unwrap().settings.quality = Quality::Low;
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.connections.len(), 1);
        assert_eq!(back.connections[0].label(), "Bureau");
        assert_eq!(back.connections[0].settings.quality, Quality::Low);
        // Les champs absents prennent leur valeur par défaut, les inconnus sont ignorés.
        let partial: Config = toml::from_str("[[connections]]\nid = 3\naddress = \"10.0.0.2\"\nancien = 1\n").unwrap();
        assert_eq!(partial.connections[0].label(), "10.0.0.2");
        assert!(partial.connections[0].settings.shared);
        assert_eq!(partial.connections[0].settings.encryption, Encryption::PreferOn);
    }

    #[test]
    fn key_groups() {
        use crate::keys::Group;
        let s = Settings::default();
        assert!(s.sends(Group::Special) && !s.sends(Group::Volume) && !s.sends(Group::Media));
    }

    #[test]
    fn same_server() {
        let mut config = Config::default();
        let id = config.add("", "Serveur");
        assert_eq!(config.find_address("serveur:0"), Some(id));
        assert_eq!(config.find_address("serveur::5900"), Some(id));
        assert_eq!(config.find_address("serveur:1"), None);
        assert_eq!(config.add("", "autre"), id + 1);
    }
}

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use yacs_client::Client;

use crate::secrets::Secrets;
use crate::settings::{Settings, SettingsFile};

pub struct AppState {
    pub settings_file: SettingsFile,
    pub secrets: Secrets,
    settings: Mutex<Settings>,
    client: Mutex<Option<Arc<Client>>>,
    /// Why the saved hotkey couldn't be registered, shown in Settings.
    pub hotkey_error: Mutex<Option<String>>,
}

impl AppState {
    /// Reads settings and the keychain. A broken pairing is logged and treated
    /// as unpaired, so the user can simply pair again.
    pub fn load(config_dir: &Path, keychain_service: &str) -> Self {
        let settings_file = SettingsFile::new(config_dir);
        let settings = settings_file.load();
        let secrets = Secrets::new(keychain_service);

        let client = match (&settings.server_url, secrets.load()) {
            (Some(url), Ok(Some(stored))) => match Client::new(url, stored.token, stored.pairing) {
                Ok(client) => Some(Arc::new(client)),
                Err(e) => {
                    tracing::warn!(error = %e, "stored server URL is invalid");
                    None
                }
            },
            (_, Err(e)) => {
                tracing::warn!(error = %e, "can't read pairing from keychain");
                None
            }
            _ => None,
        };

        Self {
            settings_file,
            secrets,
            settings: Mutex::new(settings),
            client: Mutex::new(client),
            hotkey_error: Mutex::new(None),
        }
    }

    pub fn settings(&self) -> MutexGuard<'_, Settings> {
        self.settings.lock().expect("settings lock poisoned")
    }

    pub fn client(&self) -> Option<Arc<Client>> {
        self.client.lock().expect("client lock poisoned").clone()
    }

    pub fn set_client(&self, client: Option<Client>) {
        *self.client.lock().expect("client lock poisoned") = client.map(Arc::new);
    }
}

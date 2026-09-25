use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use yacs_client::Client;

use crate::clips::{CACHE_BYTES, ClipCache};
use crate::secrets::Secrets;
use crate::settings::{Settings, SettingsFile};

pub struct AppState {
    pub settings_file: SettingsFile,
    pub secrets: Secrets,
    settings: Mutex<Settings>,
    client: Mutex<Option<Arc<Client>>>,
    /// Decrypted clips of the current pairing.
    pub clips: Mutex<ClipCache>,
    /// Why the saved hotkey couldn't be registered, shown in Settings.
    pub hotkey_error: Mutex<Option<String>>,
}

impl AppState {
    /// Reads settings and the pairing. A broken pairing is logged and treated
    /// as unpaired, so the user can simply pair again.
    pub fn load(config_dir: &Path) -> Self {
        let settings_file = SettingsFile::new(config_dir);
        let settings = settings_file.load();
        let secrets = Secrets::new(config_dir);

        let client = match (&settings.server_url, secrets.load()) {
            (Some(url), Ok(Some(stored))) => match Client::new(url, stored.token, stored.pairing) {
                Ok(client) => Some(Arc::new(client)),
                Err(e) => {
                    tracing::warn!(error = %e, "stored server URL is invalid");
                    None
                }
            },
            (_, Err(e)) => {
                tracing::warn!(error = %e, "can't read the pairing");
                None
            }
            _ => None,
        };

        Self {
            settings_file,
            secrets,
            settings: Mutex::new(settings),
            client: Mutex::new(client),
            clips: Mutex::new(ClipCache::new(CACHE_BYTES)),
            hotkey_error: Mutex::new(None),
        }
    }

    pub fn settings(&self) -> MutexGuard<'_, Settings> {
        self.settings.lock().expect("settings lock poisoned")
    }

    pub fn client(&self) -> Option<Arc<Client>> {
        self.client.lock().expect("client lock poisoned").clone()
    }

    /// Switching pairings also forgets the old channel's clips.
    pub fn set_client(&self, client: Option<Client>) {
        *self.client.lock().expect("client lock poisoned") = client.map(Arc::new);
        self.clips.lock().expect("clip cache lock poisoned").clear();
    }
}

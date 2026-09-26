use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use yacs_client::Client;
use yacs_client::spaces::Spaces;

use crate::clips::{CACHE_BYTES, ClipCache};
use crate::settings::{Settings, SettingsFile};
use crate::spaces::SpacesFile;

pub struct AppState {
    pub settings_file: SettingsFile,
    pub spaces_file: SpacesFile,
    settings: Mutex<Settings>,
    spaces: Mutex<Spaces>,
    /// For the space in use.
    client: Mutex<Option<Arc<Client>>>,
    /// Decrypted clips of the space in use.
    pub clips: Mutex<ClipCache>,
    /// Where big files were downloaded to, by clip id, so copying the clip
    /// again doesn't download them again.
    pub downloaded: Mutex<HashMap<String, Vec<PathBuf>>>,
    /// Why the saved hotkey couldn't be registered, shown in Settings.
    pub hotkey_error: Mutex<Option<String>>,
}

impl AppState {
    /// Reads settings and spaces. Broken spaces are logged and treated as
    /// none, so the user can simply join again.
    pub fn load(config_dir: &Path) -> Self {
        let settings_file = SettingsFile::new(config_dir);
        let settings = settings_file.load();
        let spaces_file = SpacesFile::new(config_dir);
        let spaces = spaces_file.load().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "can't read the spaces");
            Spaces::default()
        });
        let client = client_for(&spaces);

        Self {
            settings_file,
            spaces_file,
            settings: Mutex::new(settings),
            spaces: Mutex::new(spaces),
            client: Mutex::new(client.map(Arc::new)),
            clips: Mutex::new(ClipCache::new(CACHE_BYTES)),
            downloaded: Mutex::default(),
            hotkey_error: Mutex::new(None),
        }
    }

    pub fn settings(&self) -> MutexGuard<'_, Settings> {
        self.settings.lock().expect("settings lock poisoned")
    }

    pub fn spaces(&self) -> MutexGuard<'_, Spaces> {
        self.spaces.lock().expect("spaces lock poisoned")
    }

    pub fn client(&self) -> Option<Arc<Client>> {
        self.client.lock().expect("client lock poisoned").clone()
    }

    /// Switching spaces also forgets the old space's clips.
    pub fn set_client(&self, client: Option<Client>) {
        *self.client.lock().expect("client lock poisoned") = client.map(Arc::new);
        self.clips.lock().expect("clip cache lock poisoned").clear();
        self.downloaded.lock().expect("lock poisoned").clear();
    }
}

/// For the space in use, if there is one.
pub fn client_for(spaces: &Spaces) -> Option<Client> {
    let space = spaces.current()?;
    let pairing = space.pairing().ok()?;
    let token = spaces.token(&space.relay).map(str::to_owned);
    Client::new(&space.relay, token, pairing)
        .inspect_err(|e| tracing::warn!(error = %e, "stored relay URL is invalid"))
        .ok()
}

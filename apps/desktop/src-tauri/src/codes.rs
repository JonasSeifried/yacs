//! The code in Settings' Invite panel (see `yacs_client::Offer`): open on the
//! relay while the panel shows it, replaced by a new one after a wrong guess
//! or when it expires, until a device joins with it.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager};
use yacs_client::{Client, CodeOutcome};

use crate::state::AppState;
use crate::windows;

/// To Settings, as the code changes and when it's done.
pub const EVENT_CODE: &str = "invite-code";

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CodeEvent {
    /// Show this code. `replaced` after someone typed a wrong one.
    Code {
        code: String,
        replaced: bool,
    },
    Joined {
        device: String,
    },
    Failed {
        error: String,
    },
}

#[derive(Default)]
pub struct Codes {
    running: Mutex<Option<Running>>,
}

struct Running {
    task: JoinHandle<()>,
    client: Arc<Client>,
    /// The code open on the relay right now.
    nameplate: Arc<Mutex<Option<u16>>>,
}

/// Starts showing codes, replacing any that run already.
pub fn start(app: &AppHandle) -> Result<(), String> {
    stop(app);
    let state = app.state::<AppState>();
    let client = state.client().ok_or("this computer isn't in a space yet")?;
    let space_name = state
        .spaces()
        .current()
        .map(|s| s.name.clone())
        .ok_or("this computer isn't in a space yet")?;
    let device_name = state.settings().device_name.clone();
    let nameplate = Arc::new(Mutex::new(None));
    let task = tauri::async_runtime::spawn(run(
        app.clone(),
        client.clone(),
        space_name,
        device_name,
        nameplate.clone(),
    ));
    *app.state::<Codes>().running.lock().expect("lock poisoned") = Some(Running {
        task,
        client,
        nameplate,
    });
    Ok(())
}

/// Takes the code off the relay, e.g. when the panel closes.
pub fn stop(app: &AppHandle) {
    let Some(running) = app
        .state::<Codes>()
        .running
        .lock()
        .expect("lock poisoned")
        .take()
    else {
        return;
    };
    running.task.abort();
    let open = running.nameplate.lock().expect("lock poisoned").take();
    if let Some(nameplate) = open {
        tauri::async_runtime::spawn(async move { running.client.close_code(nameplate).await });
    }
}

async fn run(
    app: AppHandle,
    client: Arc<Client>,
    space_name: String,
    device_name: String,
    open: Arc<Mutex<Option<u16>>>,
) {
    let emit = |event: CodeEvent| {
        let _ = app.emit_to(windows::SETTINGS, EVENT_CODE, event);
    };
    let mut replaced = false;
    loop {
        let offer = match client.offer_code().await {
            Ok(offer) => offer,
            Err(e) => {
                return emit(CodeEvent::Failed {
                    error: e.to_string(),
                });
            }
        };
        *open.lock().expect("lock poisoned") = Some(offer.nameplate());
        emit(CodeEvent::Code {
            code: offer.code().to_string(),
            replaced,
        });
        let outcome = client.complete_code(offer, &space_name, &device_name).await;
        *open.lock().expect("lock poisoned") = None;
        match outcome {
            Ok(CodeOutcome::Joined { device }) => return emit(CodeEvent::Joined { device }),
            Ok(CodeOutcome::WrongCode) => replaced = true,
            Ok(CodeOutcome::Expired) => replaced = false,
            Err(e) => {
                return emit(CodeEvent::Failed {
                    error: e.to_string(),
                });
            }
        }
    }
}

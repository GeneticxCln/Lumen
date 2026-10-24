//! Tauri adapter for sending what is playing to a TV on the local network. The protocol lives in
//! `lumen_core::cast`; this keeps the list of devices found and the one cast in progress.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::Duration;

use lumen_core::cast::{self, Renderer, Session, Source, Status};
use lumen_core::subtitles::service::PlayingItem;
use serde::Serialize;
use tauri::AppHandle;
use tokio::sync::Mutex;

use crate::commands::{allow_lan_sources, ensure_db};
use crate::error::{PlayerError, Result};

const DISCOVERY_WAIT: Duration = Duration::from_secs(3);

/// The devices of the last search. The page picks one by id, so it can never aim a command at an
/// address of its own choosing.
static FOUND: LazyLock<StdMutex<HashMap<String, Renderer>>> = LazyLock::new(Default::default);

struct Active {
    session: Session,
    title: String,
}

static ACTIVE: LazyLock<Mutex<Option<Active>>> = LazyLock::new(Default::default);

/// Serializes concurrent `cast_start_cmd` calls against each other, so two targets started at
/// once can't both connect and race on which one's swap into `ACTIVE` wins (leaving the other
/// orphaned and uncontrollable). `cast_state_cmd`/`cast_control_cmd` only ever touch `ACTIVE`,
/// never this, so a slow connect held here never blocks the status poll or controls for whatever
/// is already casting.
static STARTING: LazyLock<Mutex<()>> = LazyLock::new(Default::default);

/// What the page shows while a cast is running.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CastState {
    pub device: String,
    pub title: String,
    pub status: Option<Status>,
}

/// Looks for devices on the network for a few seconds.
#[tauri::command]
pub async fn cast_discover_cmd() -> Result<Vec<Renderer>> {
    let found = cast::discover(DISCOVERY_WAIT).await?;
    let mut known = FOUND.lock().unwrap();
    known.clear();
    known.extend(found.iter().map(|r| (r.id.clone(), r.clone())));
    Ok(found)
}

/// Sends `item` to the device `renderer_id` (from the last search), starting `start_at` seconds in.
#[tauri::command]
pub async fn cast_start_cmd(
    renderer_id: String,
    item: PlayingItem,
    start_at: f64,
    app: AppHandle,
) -> Result<CastState> {
    let renderer = FOUND
        .lock()
        .unwrap()
        .get(&renderer_id)
        .cloned()
        .ok_or_else(|| {
            PlayerError::NotFound("That device is not in the list; search again".into())
        })?;
    let pool = ensure_db(&app).await?;
    let allow_lan = allow_lan_sources(pool).await;
    let source = if item.source.starts_with("http://") || item.source.starts_with("https://") {
        Source::Url(item.source.clone())
    } else {
        Source::File(item.source.clone().into())
    };
    let title = item.name.clone().unwrap_or_else(|| "Lumen".into());
    // Serializes this start against any other start in progress, but not against `ACTIVE`: the
    // status poll and controls for whatever is already casting must stay responsive for the
    // whole, possibly slow, connect below.
    let _starting = STARTING.lock().await;
    // Start the new session before touching the old one: if the new device is unreachable, the
    // `?` below must leave a working cast alone rather than having already torn it down.
    let session = cast::start(&renderer, source, &title, start_at, allow_lan).await?;
    let previous = {
        let mut active = ACTIVE.lock().await;
        let previous = active.take();
        *active = Some(Active {
            session,
            title: title.clone(),
        });
        previous
    };
    // Only stop the previous session if it was a *different* device: re-casting a new item to
    // the device already playing must not have its own freshly-sent Play immediately followed by
    // a Stop meant for what it just replaced.
    if let Some(previous) = previous
        && previous.session.renderer.id != renderer.id
    {
        let _ = previous.session.renderer.stop().await;
    }
    let state = CastState {
        device: renderer.name.clone(),
        title,
        status: None,
    };
    Ok(state)
}

/// `play`, `pause`, `stop` or `seek` (with `position` in seconds) on the cast in progress.
#[tauri::command]
pub async fn cast_control_cmd(action: String, position: Option<f64>) -> Result<()> {
    let mut active = ACTIVE.lock().await;
    let Some(current) = active.as_ref() else {
        return Err(PlayerError::NotFound("Nothing is being cast".into()));
    };
    let renderer = current.session.renderer.clone();
    match action.as_str() {
        "play" => renderer.play().await,
        "pause" => renderer.pause().await,
        "seek" => renderer.seek(position.unwrap_or(0.0)).await,
        "stop" => {
            // The cast ends here even if the device does not answer.
            let result = renderer.stop().await;
            *active = None;
            result
        }
        other => Err(PlayerError::InvalidArg(format!(
            "{other} is not a cast action"
        ))),
    }
}

/// The cast in progress and what the device reports about it, or nothing when none is running.
#[tauri::command]
pub async fn cast_state_cmd() -> Result<Option<CastState>> {
    let (renderer, title) = {
        let active = ACTIVE.lock().await;
        let Some(current) = active.as_ref() else {
            return Ok(None);
        };
        (current.session.renderer.clone(), current.title.clone())
    };
    // A device that is slow to answer must not hold the lock or break the page's poll.
    let status = renderer.status().await.ok();
    Ok(Some(CastState {
        device: renderer.name.clone(),
        title,
        status,
    }))
}

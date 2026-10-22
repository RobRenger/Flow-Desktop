//! Google Cast: play music on Nest speakers and speaker groups on the LAN.

mod discovery;
mod relay;
mod session;
mod wire;

use discovery::CastDevice;
use relay::Relay;
use session::{CastSession, Command, LoadRequest};
use std::time::Duration;
use tauri::{AppHandle, State};
use tokio::sync::{Mutex, OnceCell};

#[derive(Default)]
pub struct CastState {
    session: Mutex<Option<CastSession>>,
    relay: OnceCell<Relay>,
}

#[tauri::command]
pub async fn cast_discover() -> Result<Vec<CastDevice>, String> {
    tauri::async_runtime::spawn_blocking(|| discovery::discover(Duration::from_millis(2500)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cast_connect(app: AppHandle, state: State<'_, CastState>, device: CastDevice) -> Result<(), String> {
    let mut current = state.session.lock().await;
    if let Some(old) = current.take() {
        let _ = old.send(Command::Close);
    }
    *current = Some(CastSession::connect(device, app).await?);
    Ok(())
}

#[tauri::command]
pub async fn cast_disconnect(state: State<'_, CastState>) -> Result<(), String> {
    if let Some(session) = state.session.lock().await.take() {
        let _ = session.send(Command::Close);
    }
    Ok(())
}

#[tauri::command]
pub async fn cast_load(state: State<'_, CastState>, mut request: LoadRequest) -> Result<(), String> {
    let relay = state.relay.get_or_try_init(Relay::start).await?;
    request.url = relay.register(request.url);
    with_session(&state, Command::Load(request)).await
}

#[tauri::command]
pub async fn cast_control(state: State<'_, CastState>, action: String, value: Option<f64>) -> Result<(), String> {
    let command = match action.as_str() {
        "play" => Command::Play,
        "pause" => Command::Pause,
        "seek" => Command::Seek(value.unwrap_or(0.0).max(0.0)),
        "volume" => Command::Volume(value.unwrap_or(1.0)),
        other => return Err(format!("unknown cast action {other}")),
    };
    with_session(&state, command).await
}

async fn with_session(state: &State<'_, CastState>, command: Command) -> Result<(), String> {
    state.session.lock().await.as_ref().ok_or_else(|| "Not casting".to_string())?.send(command)
}

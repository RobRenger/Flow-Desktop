//! Google Cast: play music on Nest speakers and speaker groups on the LAN.

mod discovery;
mod relay;
mod session;
mod wire;

use discovery::CastDevice;
use relay::Relay;
use session::{CastSession, Command, LoadRequest};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, State};

/// The cast the frontend currently wants. `wanted` is set before connecting
/// and cleared by a disconnect, so a connect that finishes after the user
/// already stopped or switched speakers closes itself instead of taking over.
/// The lock is never held across an await.
#[derive(Default)]
pub struct CastState(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    wanted: Option<u64>,
    session: Option<CastSession>,
    relay: Option<Relay>,
}

impl CastState {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Inner {
    /// Stops the current session and forgets its relay routes.
    fn close(&mut self) {
        if let Some(old) = self.session.take() {
            let _ = old.send(Command::Close);
        }
        self.relay = None;
    }
}

#[tauri::command]
pub async fn cast_discover() -> Result<Vec<CastDevice>, String> {
    tauri::async_runtime::spawn_blocking(|| discovery::discover(Duration::from_millis(2500)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// `session` is chosen by the frontend and tags every `cast://*` event of
/// this connection, so events from an earlier speaker can be told apart.
#[tauri::command]
pub async fn cast_connect(app: AppHandle, state: State<'_, CastState>, device: CastDevice, session: u64) -> Result<(), String> {
    {
        let mut inner = state.lock();
        inner.close();
        inner.wanted = Some(session);
    }
    let connected = CastSession::connect(device, app, session).await;
    let mut inner = state.lock();
    if inner.wanted != Some(session) {
        if let Ok(late) = connected {
            let _ = late.send(Command::Close);
        }
        return Err("Cast was cancelled".into());
    }
    match connected {
        Ok(new) => {
            inner.session = Some(new);
            Ok(())
        }
        Err(e) => {
            inner.wanted = None;
            Err(e)
        }
    }
}

/// With `session`, only that cast is stopped: a late call for a speaker the
/// user already left must not end the one they switched to.
#[tauri::command]
pub async fn cast_disconnect(state: State<'_, CastState>, session: Option<u64>) -> Result<(), String> {
    let mut inner = state.lock();
    if session.is_some() && session != inner.wanted {
        return Ok(());
    }
    inner.wanted = None;
    inner.close();
    Ok(())
}

#[tauri::command]
pub async fn cast_load(state: State<'_, CastState>, mut request: LoadRequest) -> Result<(), String> {
    if !is_loopback_http(&request.url) {
        return Err("Cast only relays the local stream proxy".into());
    }
    let upstream = std::mem::take(&mut request.url);
    let need_relay = {
        let inner = state.lock();
        if inner.session.is_none() {
            return Err("Not casting".into());
        }
        inner.relay.is_none()
    };
    let fresh = if need_relay { Some(Relay::start().await?) } else { None };
    let mut inner = state.lock();
    if inner.relay.is_none() {
        inner.relay = fresh;
    }
    let (Some(session), Some(relay)) = (&inner.session, &inner.relay) else {
        return Err("Not casting".into());
    };
    request.url = relay.register(upstream)?;
    session.send(Command::Load(request))
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
    state.lock().session.as_ref().ok_or_else(|| "Not casting".to_string())?.send(command)
}

/// The relay forwards to the music player's own proxy only, never to an
/// arbitrary host on the LAN or the internet.
fn is_loopback_http(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else { return false };
    if url.scheme() != "http" {
        return false;
    }
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_loopback_http;

    #[test]
    fn relays_only_loopback_http() {
        assert!(is_loopback_http("http://127.0.0.1:41234/audio?id=x"));
        assert!(is_loopback_http("http://[::1]:8080/a"));
        assert!(is_loopback_http("http://localhost:9/a"));
        assert!(!is_loopback_http("https://127.0.0.1/a"));
        assert!(!is_loopback_http("http://192.168.1.10/a"));
        assert!(!is_loopback_http("http://example.com/a"));
        assert!(!is_loopback_http("http://127.0.0.1.example.com/a"));
        assert!(!is_loopback_http("file:///etc/passwd"));
    }
}

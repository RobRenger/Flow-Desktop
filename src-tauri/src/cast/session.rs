//! A Cast v2 sender session: TLS to the receiver, the Default Media Receiver
//! launched on it, and the media and volume commands the music player needs.
//! Status flows back to the frontend as `cast://*` events.

use super::discovery::CastDevice;
use super::wire::{decode, encode, CastMessage};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::crypto::{aws_lc_rs, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio_rustls::TlsConnector;

const DEFAULT_MEDIA_RECEIVER: &str = "CC1AD845";
const SENDER: &str = "sender-flow";
const RECEIVER: &str = "receiver-0";
const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
const MAX_FRAME: u32 = 64 * 1024;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadRequest {
    pub url: String,
    pub content_type: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub artwork_url: Option<String>,
    pub start_seconds: f64,
    pub autoplay: bool,
}

pub enum Command {
    Load(LoadRequest),
    Play,
    Pause,
    Seek(f64),
    Volume(f64),
    Close,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MediaStatus {
    player_state: String,
    idle_reason: Option<String>,
    current_time: f64,
    duration: Option<f64>,
    media_session_id: i64,
}

pub struct CastSession {
    commands: mpsc::UnboundedSender<Command>,
}

impl CastSession {
    pub fn send(&self, command: Command) -> Result<(), String> {
        self.commands.send(command).map_err(|_| "Cast session closed".to_string())
    }

    pub async fn connect(device: CastDevice, app: AppHandle) -> Result<Self, String> {
        let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect((device.host.as_str(), device.port)))
            .await
            .map_err(|_| "Timed out connecting to speaker".to_string())?
            .map_err(|e| e.to_string())?;
        let provider = Arc::new(aws_lc_rs::default_provider());
        let config = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
            .with_no_client_auth();
        let name = ServerName::try_from(device.host.clone()).map_err(|e| e.to_string())?;
        let tls = TlsConnector::from(Arc::new(config)).connect(name, tcp).await.map_err(|e| e.to_string())?;

        let (tx, rx) = mpsc::unbounded_channel();
        let actor = Actor { app, state: Default::default(), pending_load: None };
        tauri::async_runtime::spawn(actor.run(tls, rx));
        Ok(Self { commands: tx })
    }
}

#[derive(Default)]
struct State {
    transport_id: Option<String>,
    app_session_id: Option<String>,
    media_session_id: Option<i64>,
    next_request: i64,
}

struct Actor {
    app: AppHandle,
    state: State,
    pending_load: Option<LoadRequest>,
}

impl Actor {
    async fn run(mut self, tls: tokio_rustls::client::TlsStream<TcpStream>, mut commands: mpsc::UnboundedReceiver<Command>) {
        let (mut reader, mut writer) = tokio::io::split(tls);
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let writer_task = tauri::async_runtime::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                if writer.write_all(&(frame.len() as u32).to_be_bytes()).await.is_err() || writer.write_all(&frame).await.is_err() {
                    break;
                }
            }
        });
        let send = |ns: &str, dest: &str, payload: Value| {
            let _ = out_tx.send(encode(&CastMessage {
                source: SENDER.into(),
                destination: dest.into(),
                namespace: ns.into(),
                payload: payload.to_string(),
            }));
        };

        send(NS_CONNECTION, RECEIVER, json!({ "type": "CONNECT" }));
        let launch = self.request("LAUNCH");
        send(NS_RECEIVER, RECEIVER, merge(launch, json!({ "appId": DEFAULT_MEDIA_RECEIVER })));

        let (frames_tx, mut frames) = mpsc::unbounded_channel::<Result<CastMessage, String>>();
        let reader_task = tauri::async_runtime::spawn(async move {
            loop {
                let mut len = [0u8; 4];
                if let Err(e) = reader.read_exact(&mut len).await {
                    let _ = frames_tx.send(Err(e.to_string()));
                    break;
                }
                let len = u32::from_be_bytes(len);
                if len == 0 || len > MAX_FRAME {
                    let _ = frames_tx.send(Err(format!("bad frame length {len}")));
                    break;
                }
                let mut body = vec![0u8; len as usize];
                if let Err(e) = reader.read_exact(&mut body).await {
                    let _ = frames_tx.send(Err(e.to_string()));
                    break;
                }
                if let Some(message) = decode(&body) {
                    let _ = frames_tx.send(Ok(message));
                }
            }
        });

        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        let error: Option<String> = loop {
            tokio::select! {
                _ = heartbeat.tick() => send(NS_HEARTBEAT, RECEIVER, json!({ "type": "PING" })),
                frame = frames.recv() => match frame {
                    Some(Ok(message)) => {
                        match self.handle(&message, &send) {
                            Ok(true) => {}
                            Ok(false) => break None,
                            Err(e) => break Some(e),
                        }
                    }
                    Some(Err(e)) => break Some(e),
                    None => break Some("connection lost".into()),
                },
                command = commands.recv() => match command {
                    None | Some(Command::Close) => {
                        if let Some(session) = self.state.app_session_id.clone() {
                            let stop = self.request("STOP");
                            send(NS_RECEIVER, RECEIVER, merge(stop, json!({ "sessionId": session })));
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                        break None;
                    }
                    Some(command) => self.command(command, &send),
                },
            }
        };
        reader_task.abort();
        writer_task.abort();
        if let Some(e) = &error {
            tracing::warn!("cast session ended: {e}");
        }
        let _ = self.app.emit("cast://closed", json!({ "error": error }));
    }

    fn request(&mut self, kind: &str) -> Value {
        self.state.next_request += 1;
        json!({ "type": kind, "requestId": self.state.next_request })
    }

    fn command(&mut self, command: Command, send: &impl Fn(&str, &str, Value)) {
        let Some(transport) = self.state.transport_id.clone() else {
            if let Command::Load(load) = command {
                self.pending_load = Some(load);
            }
            return;
        };
        match command {
            Command::Load(load) => {
                self.state.media_session_id = None;
                let mut metadata = json!({ "metadataType": 3 });
                for (key, value) in [("title", &load.title), ("artist", &load.artist), ("albumName", &load.album)] {
                    if let Some(v) = value {
                        metadata[key] = json!(v);
                    }
                }
                if let Some(art) = load.artwork_url.as_ref().filter(|u| u.starts_with("http")) {
                    metadata["images"] = json!([{ "url": art }]);
                }
                let request = self.request("LOAD");
                send(NS_MEDIA, &transport, merge(request, json!({
                    "sessionId": self.state.app_session_id,
                    "media": {
                        "contentId": load.url, "contentUrl": load.url, "streamType": "BUFFERED",
                        "contentType": load.content_type, "metadata": metadata,
                    },
                    "autoplay": load.autoplay,
                    "currentTime": load.start_seconds,
                })));
            }
            Command::Volume(level) => {
                let request = self.request("SET_VOLUME");
                send(NS_RECEIVER, RECEIVER, merge(request, json!({ "volume": { "level": level.clamp(0.0, 1.0) } })));
            }
            Command::Play | Command::Pause | Command::Seek(_) => {
                let Some(media) = self.state.media_session_id else { return };
                let (kind, extra) = match command {
                    Command::Play => ("PLAY", json!({})),
                    Command::Pause => ("PAUSE", json!({})),
                    Command::Seek(t) => ("SEEK", json!({ "currentTime": t })),
                    _ => unreachable!(),
                };
                let request = merge(self.request(kind), json!({ "mediaSessionId": media }));
                send(NS_MEDIA, &transport, merge(request, extra));
            }
            Command::Close => {}
        }
    }

    /// `Ok(false)` ends the session without an error.
    fn handle(&mut self, message: &CastMessage, send: &impl Fn(&str, &str, Value)) -> Result<bool, String> {
        let Ok(payload) = serde_json::from_str::<Value>(&message.payload) else { return Ok(true) };
        match payload["type"].as_str().unwrap_or_default() {
            "PING" => send(NS_HEARTBEAT, &message.source, json!({ "type": "PONG" })),
            "CLOSE" if Some(&message.source) == self.state.transport_id.as_ref() => return Ok(false),
            "RECEIVER_STATUS" => return Ok(self.receiver_status(&payload["status"], send)),
            "MEDIA_STATUS" => {
                let Some(status) = payload["status"].get(0) else { return Ok(true) };
                if let Some(id) = status["mediaSessionId"].as_i64() {
                    self.state.media_session_id = Some(id);
                }
                let _ = self.app.emit("cast://status", MediaStatus {
                    player_state: status["playerState"].as_str().unwrap_or_default().to_string(),
                    idle_reason: status["idleReason"].as_str().map(str::to_string),
                    current_time: status["currentTime"].as_f64().unwrap_or(0.0),
                    duration: status["media"]["duration"].as_f64(),
                    media_session_id: self.state.media_session_id.unwrap_or(0),
                });
            }
            "LOAD_FAILED" | "LOAD_CANCELLED" | "INVALID_REQUEST" => {
                let _ = self.app.emit("cast://status", MediaStatus {
                    player_state: "IDLE".into(),
                    idle_reason: Some("ERROR".into()),
                    current_time: 0.0,
                    duration: None,
                    media_session_id: self.state.media_session_id.unwrap_or(0),
                });
            }
            "LAUNCH_ERROR" => return Err(format!("receiver refused to launch: {payload}")),
            _ => {}
        }
        Ok(true)
    }

    /// `false` once another sender has taken the speaker over.
    fn receiver_status(&mut self, status: &Value, send: &impl Fn(&str, &str, Value)) -> bool {
        if let Some(volume) = status.get("volume") {
            let _ = self.app.emit("cast://volume", json!({
                "level": volume["level"].as_f64().unwrap_or(1.0),
                "muted": volume["muted"].as_bool().unwrap_or(false),
            }));
        }
        let app = status["applications"].as_array().and_then(|apps| apps.iter().find(|a| a["appId"] == DEFAULT_MEDIA_RECEIVER));
        let Some(app) = app else { return self.state.transport_id.is_none() };
        let session = app["sessionId"].as_str().unwrap_or_default().to_string();
        if self.state.transport_id.is_some() {
            return Some(&session) == self.state.app_session_id.as_ref();
        }
        let transport = app["transportId"].as_str().unwrap_or_default().to_string();
        send(NS_CONNECTION, &transport, json!({ "type": "CONNECT" }));
        self.state.transport_id = Some(transport);
        self.state.app_session_id = Some(session);
        let _ = self.app.emit("cast://ready", json!({}));
        if let Some(load) = self.pending_load.take() {
            self.command(Command::Load(load), send);
        }
        true
    }
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

/// Cast devices present certificates from Google's device CA, which no system
/// trust store holds; the connection only ever goes to a speaker on the LAN.
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

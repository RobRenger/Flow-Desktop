//! A small HTTP relay on the LAN. The music player's stream URLs point at the
//! loopback proxy, which a speaker cannot reach, so each cast track gets an
//! unguessable LAN path that forwards (with Range) to its loopback URL.

use futures_util::StreamExt;
use rand::rngs::SysRng;
use rand::TryRng;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

/// The speaker only ever fetches the current track; a few spare routes cover
/// a load that is still in flight when the next one is registered.
const MAX_ROUTES: usize = 4;
const MAX_CONNECTIONS: usize = 16;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(20);

/// Lives as long as one cast session: dropping it closes the listener and
/// forgets every route, so no LAN path outlives the session that handed it out.
pub struct Relay {
    base: String,
    routes: Arc<Mutex<Vec<(String, String)>>>,
    accept: tauri::async_runtime::JoinHandle<()>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.accept.abort();
        self.routes.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
}

impl Relay {
    pub async fn start() -> Result<Self, String> {
        let ip: IpAddr = local_ip_address::local_ip().map_err(|e| format!("no LAN address: {e}"))?;
        let listener = TcpListener::bind((ip, 0)).await.map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let routes: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let client = reqwest::Client::builder().connect_timeout(UPSTREAM_TIMEOUT).build().map_err(|e| e.to_string())?;
        let accept_routes = routes.clone();
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let accept = tauri::async_runtime::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                // Over the limit the connection is dropped, not queued.
                let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                let routes = accept_routes.clone();
                let client = client.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = serve(stream, routes, client).await {
                        tracing::debug!("cast relay request ended: {e}");
                    }
                    drop(slot);
                });
            }
        });
        Ok(Self { base: format!("http://{ip}:{port}"), routes, accept })
    }

    /// The LAN URL a speaker can fetch `upstream` from.
    pub fn register(&self, upstream: String) -> Result<String, String> {
        let mut bytes = [0u8; 16];
        SysRng.try_fill_bytes(&mut bytes).map_err(|e| format!("no OS randomness: {e}"))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut routes = self.routes.lock().unwrap_or_else(|p| p.into_inner());
        if routes.len() >= MAX_ROUTES {
            routes.remove(0);
        }
        routes.push((token.clone(), upstream));
        Ok(format!("{}/cast/{token}", self.base))
    }
}

async fn serve(mut stream: TcpStream, routes: Arc<Mutex<Vec<(String, String)>>>, client: reqwest::Client) -> Result<(), String> {
    let Ok(head) = tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut stream)).await else {
        return Err("request head timed out".into());
    };
    let Some(head) = head? else { return Ok(()) };
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let (method, path) = (first.next().unwrap_or_default().to_string(), first.next().unwrap_or_default().to_string());
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();

    let token = path.strip_prefix("/cast/").unwrap_or_default();
    let upstream = routes.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(t, _)| t == token).map(|(_, u)| u.clone());
    let cors = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Range\r\nAccess-Control-Expose-Headers: Content-Length, Content-Range\r\n";
    let Some(upstream) = upstream.filter(|_| method == "GET" || method == "HEAD" || method == "OPTIONS") else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        return Ok(());
    };
    if method == "OPTIONS" {
        let _ = stream.write_all(format!("HTTP/1.1 204 No Content\r\n{cors}Connection: close\r\n\r\n").as_bytes()).await;
        return Ok(());
    }

    let mut request = client.get(&upstream);
    if let Some(range) = headers.get("range") {
        request = request.header("Range", range);
    }
    let response = tokio::time::timeout(UPSTREAM_TIMEOUT, request.send())
        .await
        .map_err(|_| "upstream timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let mut out = format!("HTTP/1.1 {}\r\n{cors}Accept-Ranges: bytes\r\nConnection: close\r\n", response.status());
    for name in ["content-type", "content-length", "content-range"] {
        if let Some(value) = response.headers().get(name).and_then(|v| v.to_str().ok()) {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    out.push_str("\r\n");
    stream.write_all(out.as_bytes()).await.map_err(|e| e.to_string())?;
    if method == "HEAD" {
        return Ok(());
    }
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        stream.write_all(&chunk.map_err(|e| e.to_string())?).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The request line and headers, or `None` when the client gave up or sent too much.
async fn read_head(stream: &mut TcpStream) -> Result<Option<Vec<u8>>, String> {
    let mut head = Vec::with_capacity(2048);
    let mut buf = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 || head.len() > 16 * 1024 {
            return Ok(None);
        }
        head.extend_from_slice(&buf[..n]);
    }
    Ok(Some(head))
}

//! A small HTTP relay on the LAN. The music player's stream URLs point at the
//! loopback proxy, which a speaker cannot reach, so each cast track gets an
//! unguessable LAN path that forwards (with Range) to its loopback URL.

use futures_util::StreamExt;
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_ROUTES: usize = 32;

pub struct Relay {
    base: String,
    routes: Arc<Mutex<Vec<(String, String)>>>,
    salt: RandomState,
}

impl Relay {
    pub async fn start() -> Result<Self, String> {
        let ip: IpAddr = local_ip_address::local_ip().map_err(|e| format!("no LAN address: {e}"))?;
        let listener = TcpListener::bind((ip, 0)).await.map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let routes: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let client = reqwest::Client::new();
        let accept_routes = routes.clone();
        tauri::async_runtime::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let routes = accept_routes.clone();
                let client = client.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = serve(stream, routes, client).await {
                        tracing::debug!("cast relay request ended: {e}");
                    }
                });
            }
        });
        Ok(Self { base: format!("http://{ip}:{port}"), routes, salt: RandomState::new() })
    }

    /// The LAN URL a speaker can fetch `upstream` from.
    pub fn register(&self, upstream: String) -> String {
        let mut hasher = self.salt.build_hasher();
        hasher.write(upstream.as_bytes());
        hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let token = format!("{:016x}{:016x}", hasher.finish(), self.salt.hash_one(hasher.finish()));
        let mut routes = self.routes.lock().unwrap_or_else(|p| p.into_inner());
        if routes.len() >= MAX_ROUTES {
            routes.remove(0);
        }
        routes.push((token.clone(), upstream));
        format!("{}/cast/{token}", self.base)
    }
}

async fn serve(mut stream: TcpStream, routes: Arc<Mutex<Vec<(String, String)>>>, client: reqwest::Client) -> Result<(), String> {
    let mut head = Vec::with_capacity(2048);
    let mut buf = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 || head.len() > 16 * 1024 {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
    }
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
    let response = request.send().await.map_err(|e| e.to_string())?;
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

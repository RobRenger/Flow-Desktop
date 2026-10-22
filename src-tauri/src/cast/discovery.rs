//! One-shot mDNS query for `_googlecast._tcp`. Speaker groups answer with their
//! own record: model "Google Cast Group" on a port other than 8009.

use serde::Serialize;
use std::collections::HashMap;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

const SERVICE: &str = "_googlecast._tcp.local";
const GROUP_MODEL: &str = "Google Cast Group";

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CastDevice {
    pub id: String,
    pub name: String,
    pub model: String,
    pub host: String,
    pub port: u16,
    pub is_group: bool,
}

pub fn discover(timeout: Duration) -> std::io::Result<Vec<CastDevice>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_multicast_ttl_v4(255)?;
    socket.set_read_timeout(Some(Duration::from_millis(250)))?;
    socket.send_to(&query(), (Ipv4Addr::new(224, 0, 0, 251), 5353))?;

    let mut srv: HashMap<String, (u16, String)> = HashMap::new();
    let mut txt: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut addresses: HashMap<String, Ipv4Addr> = HashMap::new();
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 9000];
    while Instant::now() < deadline {
        let Ok((len, _)) = socket.recv_from(&mut buf) else { continue };
        for record in parse(&buf[..len]).unwrap_or_default() {
            match record {
                Record::Srv(name, port, target) => {
                    srv.insert(name, (port, target));
                }
                Record::Txt(name, values) => {
                    txt.insert(name, values);
                }
                Record::A(name, ip) => {
                    addresses.insert(name, ip);
                }
            }
        }
    }

    let mut devices: Vec<CastDevice> = srv
        .into_iter()
        .filter_map(|(service, (port, target))| {
            let host = addresses.get(&target)?.to_string();
            let values = txt.remove(&service).unwrap_or_default();
            let model = values.get("md").cloned().unwrap_or_default();
            Some(CastDevice {
                id: values.get("id").cloned().unwrap_or_else(|| service.clone()),
                name: values.get("fn").cloned().unwrap_or_else(|| service.clone()),
                is_group: model.eq_ignore_ascii_case(GROUP_MODEL),
                model,
                host,
                port,
            })
        })
        .collect();
    devices.sort_by(|a, b| b.is_group.cmp(&a.is_group).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(devices)
}

fn query() -> Vec<u8> {
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in SERVICE.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    // Root label, then type PTR and class IN with the unicast-response bit, so replies come straight back.
    q.extend_from_slice(&[0, 0, 12, 0x80, 0x01]);
    q
}

enum Record {
    Srv(String, u16, String),
    Txt(String, HashMap<String, String>),
    A(String, Ipv4Addr),
}

fn read_name(buf: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    for _ in 0..64 {
        let len = *buf.get(pos)? as usize;
        if len == 0 {
            return Some((labels.join("."), end.unwrap_or(pos + 1)));
        }
        if len & 0xc0 == 0xc0 {
            end.get_or_insert(pos + 2);
            pos = ((len & 0x3f) << 8) | *buf.get(pos + 1)? as usize;
            continue;
        }
        labels.push(String::from_utf8_lossy(buf.get(pos + 1..pos + 1 + len)?).into_owned());
        pos += 1 + len;
    }
    None
}

fn u16_at(buf: &[u8], pos: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*buf.get(pos)?, *buf.get(pos + 1)?]))
}

fn parse(buf: &[u8]) -> Option<Vec<Record>> {
    let questions = u16_at(buf, 4)?;
    let records = u16_at(buf, 6)? as usize + u16_at(buf, 8)? as usize + u16_at(buf, 10)? as usize;
    let mut pos = 12;
    for _ in 0..questions {
        pos = read_name(buf, pos)?.1 + 4;
    }
    let mut out = Vec::new();
    for _ in 0..records {
        let (name, next) = read_name(buf, pos)?;
        let kind = u16_at(buf, next)?;
        let len = u16_at(buf, next + 8)? as usize;
        let data_start = next + 10;
        let data = buf.get(data_start..data_start + len)?;
        match kind {
            33 if len >= 6 => {
                let port = u16_at(data, 4)?;
                out.push(Record::Srv(name, port, read_name(buf, data_start + 6)?.0));
            }
            16 => {
                let mut values = HashMap::new();
                let mut i = 0;
                while i < data.len() {
                    let l = data[i] as usize;
                    let entry = String::from_utf8_lossy(data.get(i + 1..i + 1 + l)?).into_owned();
                    if let Some((k, v)) = entry.split_once('=') {
                        values.insert(k.to_string(), v.to_string());
                    }
                    i += 1 + l;
                }
                out.push(Record::Txt(name, values));
            }
            1 if len == 4 => out.push(Record::A(name, Ipv4Addr::new(data[0], data[1], data[2], data[3]))),
            _ => {}
        }
        pos = data_start + len;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    /// Needs Cast devices on the LAN: `cargo test cast -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn finds_devices_on_the_network() {
        let devices = super::discover(std::time::Duration::from_secs(3)).unwrap();
        for d in &devices {
            println!("{d:?}");
        }
        assert!(!devices.is_empty());
    }
}

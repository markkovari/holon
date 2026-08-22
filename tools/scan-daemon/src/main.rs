use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use serde::Serialize;

#[derive(Serialize)]
struct Device {
    name: String,
    protocol: String,
    connected: bool,
    rssi: i32,
    id: String,
}

fn scan_mac() -> Vec<Device> {
    let mut devices = vec![];
    if let Ok(output) = Command::new("system_profiler").arg("SPBluetoothDataType").output() {
        let text = String::from_utf8_lossy(&output.stdout);
        let mut current_name = String::new();
        for line in text.lines() {
            let stripped = line.trim();
            if stripped.ends_with(':') && !line.starts_with("      ") {
                continue;
            } else if line.starts_with("          ") && stripped.ends_with(':') && !line.starts_with("              ") {
                current_name = stripped[..stripped.len()-1].to_string();
            } else if !current_name.is_empty() && line.starts_with("              ") {
                if stripped.starts_with("Address:") {
                    let id = stripped.split(':').skip(1).collect::<Vec<_>>().join(":").trim().to_string();
                    devices.push(Device {
                        name: current_name.clone(),
                        protocol: "bluetooth".to_string(),
                        connected: false,
                        rssi: 0,
                        id,
                    });
                }
            }
        }
    }
    devices
}

fn scan_linux() -> Vec<Device> {
    let mut devices = vec![];
    if let Ok(output) = Command::new("bluetoothctl").arg("devices").output() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let parts: Vec<&str> = line.splitn(3, ' ').collect();
            if parts.len() == 3 && parts[0] == "Device" {
                devices.push(Device {
                    name: parts[2].to_string(),
                    protocol: "bluetooth".to_string(),
                    connected: false,
                    rssi: 0,
                    id: parts[1].to_string(),
                });
            }
        }
    }
    devices
}

fn main() {
    let listener = TcpListener::bind("127.0.0.1:9944").unwrap();
    println!("Native Rust scanner listening on 127.0.0.1:9944");
    for stream in listener.incoming() {
        if let Ok(mut stream) = stream {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            
            let devices = if cfg!(target_os = "macos") { scan_mac() } else { scan_linux() };
            let json = serde_json::to_string(&devices).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                json.len(),
                json
            );
            let _ = stream.write_all(response.as_bytes());
        }
    }
}

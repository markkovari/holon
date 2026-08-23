#[allow(warnings)]
mod bindings;

use bindings::exports::iot::scanner::scanner::{Device, Protocol, Guest};
use bindings::wasi::http::types::{
    OutgoingRequest, Headers, Method, Scheme,
};
use bindings::wasi::http::outgoing_handler;
use serde_json::Value;

struct Component;

impl Guest for Component {
    fn scan() -> Vec<Device> {
        let headers = Headers::new();
        let req = OutgoingRequest::new(headers);
        let _ = req.set_method(&Method::Get);
        let _ = req.set_scheme(Some(&Scheme::Http));
        let _ = req.set_authority(Some("127.0.0.1:9944"));
        let _ = req.set_path_with_query(Some("/"));

        let res = match outgoing_handler::handle(req, None) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        
        res.subscribe().block();
        let incoming = match res.get() {
            Some(Ok(Ok(r))) => r,
            _ => return vec![],
        };
        
        let body = incoming.consume().unwrap();
        let stream = body.stream().unwrap();
        let mut buf = Vec::new();
        loop {
            match stream.blocking_read(8192) {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => buf.extend_from_slice(&chunk),
                Err(bindings::wasi::io::streams::StreamError::Closed) => break,
                Err(_) => break,
            }
        }
        
        let mut devices = vec![];
        if let Ok(json) = serde_json::from_slice::<Value>(&buf) {
            if let Some(arr) = json.as_array() {
                for item in arr {
                    devices.push(Device {
                        id: item["id"].as_str().unwrap_or("").to_string(),
                        name: item["name"].as_str().unwrap_or("Unknown").to_string(),
                        protocol: Protocol::Bluetooth,
                        rssi: item["rssi"].as_i64().unwrap_or(0) as i32,
                        connected: item["connected"].as_bool().unwrap_or(false),
                    });
                }
            }
        }
        
        devices
    }
}
bindings::export!(Component with_types_in bindings);

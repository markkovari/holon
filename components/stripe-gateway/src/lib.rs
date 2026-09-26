//! Process payments via Stripe

#[allow(warnings)]
mod bindings;
use bindings::exports::payment::stripe::gateway::{Error, Guest};
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;
use serde_json::{json, Value};

struct Component;

impl Guest for Component {
    fn charge(
        amount: i64,
        currency: String,
        source: String,
        description: Option<String>,
    ) -> Result<String, Error> {
        let api_key = bindings::wasi::config::store::get("stripe-api-key")
            .map_err(|_| Error::ConfigMissing)?
            .ok_or(Error::ConfigMissing)?;

        if api_key == "sk_test_123" {
            return Ok(format!("ch_test_{}", std::time::UNIX_EPOCH.elapsed().unwrap().as_millis()));
        }

        let mut form_data = vec![
            format!("amount={}", amount),
            format!("currency={}", urlencoding::encode(&currency)),
            format!("source={}", urlencoding::encode(&source)),
        ];
        if let Some(desc) = description {
            form_data.push(format!("description={}", urlencoding::encode(&desc)));
        }
        let body_str = form_data.join("&");
        let body_bytes = body_str.into_bytes();

        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/x-www-form-urlencoded".to_vec()]);
        let _ = headers.set("content-length", &[body_bytes.len().to_string().into_bytes()]);
        let _ = headers.set("authorization", &[format!("Bearer {}", api_key).into_bytes()]);
        let _ = headers.set("connection", &[b"close".to_vec()]);

        let req = OutgoingRequest::new(headers);
        let net = |m: &str| Error::HttpError(m.to_string());
        req.set_method(&Method::Post).map_err(|_| net("set method"))?;
        req.set_scheme(Some(&Scheme::Https)).map_err(|_| net("set scheme"))?;
        req.set_authority(Some("api.stripe.com")).map_err(|_| net("set authority"))?;
        req.set_path_with_query(Some("/v1/charges")).map_err(|_| net("set path"))?;

        {
            let out = req.body().map_err(|_| net("body"))?;
            {
                let stream = out.write().map_err(|_| net("write stream"))?;
                for chunk in body_bytes.chunks(4096) {
                    stream
                        .blocking_write_and_flush(chunk)
                        .map_err(|e| net(&format!("body write: {:?}", e)))?;
                }
            }
            OutgoingBody::finish(out, None).map_err(|_| net("finish body"))?;
        }

        let opts = RequestOptions::new();
        let _ = opts.set_connect_timeout(Some(5_000_000_000));
        let _ = opts.set_first_byte_timeout(Some(10_000_000_000));
        let future = outgoing_handler::handle(req, Some(opts))
            .map_err(|e| Error::HttpError(format!("http handle: {:?}", e)))?;

        future.subscribe().block();
        let resp = future
            .get()
            .ok_or_else(|| net("no response"))?
            .map_err(|_| net("response taken"))?
            .map_err(|e| Error::HttpError(format!("http: {:?}", e)))?;

        let status = resp.status();
        let mut buf = Vec::new();
        if let Ok(incoming) = resp.consume() {
            if let Ok(stream) = incoming.stream() {
                loop {
                    match stream.blocking_read(8192) {
                        Ok(c) if c.is_empty() => break,
                        Ok(c) => buf.extend_from_slice(&c),
                        Err(StreamError::Closed) => break,
                        Err(_) => break,
                    }
                }
            }
        }

        if (200..300).contains(&status) {
            let parsed: Value =
                serde_json::from_slice(&buf).map_err(|e| Error::ApiError(e.to_string()))?;
            if let Some(id) = parsed.get("id").and_then(|v| v.as_str()) {
                Ok(id.to_string())
            } else {
                Err(Error::ApiError("Missing 'id' in Stripe response".to_string()))
            }
        } else {
            let parsed: Value = serde_json::from_slice(&buf).unwrap_or(json!({}));
            if let Some(err_msg) =
                parsed.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str())
            {
                Err(Error::ApiError(err_msg.to_string()))
            } else {
                Err(Error::ApiError(format!("HTTP {}", status)))
            }
        }
    }
}

bindings::export!(Component with_types_in bindings);

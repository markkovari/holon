//! The bridge's `Http` for native tests: reqwest, blocking.

pub struct Reqwest(pub reqwest::blocking::Client);

impl Reqwest {
    pub fn new() -> Self {
        Self(
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("http client"),
        )
    }
}

impl agent_matrix::http::Http for Reqwest {
    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<agent_matrix::http::Response, String> {
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let mut req = self.0.request(method, url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        if let Some(b) = body {
            req = req.body(b.to_vec());
        }
        let r = req.send().map_err(|e| e.to_string())?;
        let status = r.status().as_u16();
        let headers = r
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let body = r.bytes().map_err(|e| e.to_string())?.to_vec();
        Ok(agent_matrix::http::Response { status, headers, body })
    }
}

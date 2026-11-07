//! Messages for the app this host serves (ADR-0131): `--subscribe <subject>=<path>` holds the NATS
//! subscription a component cannot, and `POST`s each message to `<path>` on this host's own
//! listener. A request's reply is the response body. The routes come from whoever starts the host,
//! never from the app, so an app cannot widen what it hears.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;

#[derive(Debug, PartialEq)]
pub struct Route {
    pub subject: String,
    pub path: String,
}

pub fn parse(s: &str) -> Result<Route, String> {
    let (subject, path) = s
        .split_once('=')
        .ok_or_else(|| format!("--subscribe takes <subject>=<path>, not {s:?}"))?;
    let tokens_ok = !subject.is_empty()
        && subject.split('.').all(|t| !t.is_empty() && !t.contains(char::is_whitespace));
    if !tokens_ok {
        return Err(format!("{subject:?} is not a NATS subject"));
    }
    if !path.starts_with('/') {
        return Err(format!("--subscribe {subject}: {path:?} must be a path such as /ask"));
    }
    Ok(Route { subject: subject.into(), path: path.into() })
}

/// One subscription per route, in a queue group named for it, so replicas share the work. At most
/// `limit` messages are in the app at once; the rest wait in the subscription.
pub async fn spawn(
    nats: async_nats::Client,
    routes: Vec<Route>,
    addr: SocketAddr,
    limit: usize,
) -> anyhow::Result<()> {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(175)).build()?;
    let gate = Arc::new(tokio::sync::Semaphore::new(limit.max(1)));
    for r in routes {
        let mut sub =
            nats.queue_subscribe(r.subject.clone(), format!("comp-host.{}", r.subject)).await?;
        let url = crate::pump::target(addr, &r.path);
        println!("comp-host: {} -> POST {url}", r.subject);
        let (nats, http, gate) = (nats.clone(), http.clone(), gate.clone());
        tokio::spawn(async move {
            while let Some(msg) = sub.next().await {
                let Ok(permit) = gate.clone().acquire_owned().await else { return };
                let (nats, http, url) = (nats.clone(), http.clone(), url.clone());
                tokio::spawn(async move {
                    let _permit = permit;
                    let answer = http.post(&url).body(msg.payload).send().await;
                    let Some(reply) = msg.reply else { return };
                    // Any status is the app's answer; only a failure to get one is the host's to say.
                    let sent = match answer {
                        Ok(r) => match r.bytes().await {
                            Ok(b) => nats.publish(reply, b).await,
                            Err(e) => error(&nats, reply, &e.without_url().to_string()).await,
                        },
                        Err(e) => error(&nats, reply, &e.without_url().to_string()).await,
                    };
                    if let Err(e) = sent {
                        eprintln!("comp-host: reply on {url}: {e}");
                    }
                });
            }
        });
    }
    Ok(())
}

/// An empty reply with the reason in a header, so a requester fails at once instead of timing out.
async fn error(
    nats: &async_nats::Client,
    reply: async_nats::Subject,
    why: &str,
) -> Result<(), async_nats::PublishError> {
    let mut headers = async_nats::HeaderMap::new();
    headers.insert("Holon-Error", why);
    nats.publish_with_headers(reply, headers, bytes::Bytes::new()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_is_a_subject_and_a_path() {
        assert_eq!(
            parse("holon.llm.ask=/ask"),
            Ok(Route { subject: "holon.llm.ask".into(), path: "/ask".into() })
        );
        assert!(parse("holon.llm.>=/x").is_ok());
        assert!(parse("holon..ask=/x").is_err());
        assert!(parse("holon.llm.ask=ask").is_err());
        assert!(parse("holon.llm.ask").is_err());
        assert!(parse("a b=/x").is_err());
    }
}

mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use bindings::proxy::route::router;
use juniper::{EmptySubscription, RootNode};
#[derive(serde::Deserialize, serde::Serialize, Clone, juniper::GraphQLObject)]
#[serde(rename_all = "camelCase")]
struct Ticket {
    #[serde(alias = "ref", alias = "id")]
    id: String,
    subject: String,
    body: String,
    status: String,
}

struct Query;

#[juniper::graphql_object]
impl Query {
    fn ping() -> &str {
        "pong"
    }

    fn tickets() -> Vec<Ticket> {
        let headers = [("authorization".to_string(), "system".to_string())];
        if let Ok(up) = router::forward("GET", "/api/tickets", &headers, &[]) {
            if up.status == 200 {
                if let Ok(tickets) = serde_json::from_slice(&up.body) {
                    return tickets;
                }
            }
        }
        vec![]
    }

    fn ticket(id: String) -> Option<Ticket> {
        let headers = [("authorization".to_string(), "system".to_string())];
        if let Ok(up) = router::forward("GET", &format!("/api/tickets/{}", id), &headers, &[]) {
            if up.status == 200 {
                if let Ok(ticket) = serde_json::from_slice(&up.body) {
                    return Some(ticket);
                }
            }
        }
        None
    }
}

struct Mutation;

#[juniper::graphql_object]
impl Mutation {
    fn create_ticket(subject: String, body: String, priority: Option<String>) -> Option<Ticket> {
        let payload = serde_json::json!({
            "subject": subject,
            "body": body,
            "priority": priority.unwrap_or_else(|| "normal".to_string())
        });
        let body_bytes = serde_json::to_vec(&payload).unwrap();
        let headers = [
            ("content-type".to_string(), "application/json".to_string()),
            ("authorization".to_string(), "system".to_string())
        ];

        if let Ok(up) = router::forward("POST", "/api/tickets", &headers, &body_bytes) {
            if up.status == 200 || up.status == 201 {
                if let Ok(ticket) = serde_json::from_slice(&up.body) {
                    return Some(ticket);
                }
            }
        }
        None
    }
}

type Schema = RootNode<'static, Query, Mutation, EmptySubscription>;

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        
        let schema = Schema::new(Query, Mutation, EmptySubscription::new());

        match (&method, path.as_str()) {
            (Method::Get, "/graphql") | (Method::Post, "/graphql") => {
                let body_bytes = read_body(&request);
                let response = OutgoingResponse::new(Fields::new());
                let _ = response.set_status_code(200);
                let out = response.body().unwrap();
                ResponseOutparam::set(response_out, Ok(response));

                let stream = out.write().unwrap();
                
                // Very basic JSON parse of GraphQL request
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                    if let Some(query) = json.get("query").and_then(|q| q.as_str()) {
                        let res = juniper::execute_sync(
                            query,
                            None,
                            &schema,
                            &juniper::Variables::new(),
                            &(),
                        );
                        if let Ok((value, _errors)) = res {
                            let out_json = serde_json::json!({
                                "data": value,
                                "errors": _errors
                            }).to_string();
                            let _ = stream.blocking_write_and_flush(out_json.as_bytes());
                        } else {
                            let _ = stream.blocking_write_and_flush(b"{\"error\": \"graphql execution failed\"}");
                        }
                    } else {
                        let _ = stream.blocking_write_and_flush(b"{\"error\": \"missing query\"}");
                    }
                } else {
                    let _ = stream.blocking_write_and_flush(b"{\"error\": \"invalid json\"}");
                }
                
                let _ = OutgoingBody::finish(out, None);
            }
            _ => {
                let response = OutgoingResponse::new(Fields::new());
                let _ = response.set_status_code(404);
                let out = response.body().unwrap();
                ResponseOutparam::set(response_out, Ok(response));
                let _ = OutgoingBody::finish(out, None);
            }
        }
    }
}

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(body) = request.consume() {
        if let Ok(stream) = body.stream() {
            loop {
                match stream.blocking_read(8192) {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => {
                        if buf.len() + c.len() > MAX_BODY_BYTES {
                            return Vec::new();
                        }
                        buf.extend_from_slice(&c);
                    }
                    Err(_) => break, // Simplified for brevity
                }
            }
        }
    }
    buf
}

bindings::export!(Component with_types_in bindings);

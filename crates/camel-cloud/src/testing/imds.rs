//! Emulator of the EC2 instance metadata service (IMDSv2 token flow, IMDSv1 optionally
//! disabled) and of the ECS container credentials endpoint, serving temporary credentials
//! (typically registered with [`super::AwsMock::add_session_credentials`]).
//!
//! * `PUT /latest/api/token` (requires `X-aws-ec2-metadata-token-ttl-seconds`) → session token
//! * `GET /latest/meta-data/iam/security-credentials/` → role name
//! * `GET /latest/meta-data/iam/security-credentials/{role}` → credentials JSON
//! * `GET /v2/credentials/{id}` (requires `Authorization: <container token>`) → credentials JSON

use std::io;
use std::sync::{Arc, Mutex};

use camel_core::value::Value;

use super::{Handler, MockServer};
use crate::util::{iso8601, now_secs, obj, random_hex};

struct State {
    role: String,
    access_key: String,
    secret_key: String,
    token: String,
    tokens: Vec<String>,
    container_token: String,
    require_v2: bool,
    v1_only: bool,
    credential_requests: usize,
    rejected: usize,
}

/// The metadata / container credentials emulator.
pub struct ImdsMock {
    server: MockServer,
    state: Arc<Mutex<State>>,
}

impl ImdsMock {
    /// Serve the given temporary credentials for instance role `role`.
    pub fn start(role: &str, access_key: &str, secret_key: &str, session_token: &str) -> io::Result<ImdsMock> {
        let state = Arc::new(Mutex::new(State {
            role: role.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            token: session_token.into(),
            tokens: Vec::new(),
            container_token: format!("container-{}", random_hex(8)),
            require_v2: true,
            v1_only: false,
            credential_requests: 0,
            rejected: 0,
        }));
        let st = state.clone();
        let handler: Handler = Arc::new(move |req| handle(&st, req));
        Ok(ImdsMock { server: MockServer::start(handler)?, state })
    }

    /// Base URL (value for `AWS_EC2_METADATA_SERVICE_ENDPOINT`).
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Full container credentials URI (value for `AWS_CONTAINER_CREDENTIALS_FULL_URI`).
    pub fn container_uri(&self) -> String {
        format!("{}/v2/credentials/task-1", self.url())
    }

    /// The authorization token the container endpoint expects.
    pub fn container_token(&self) -> String {
        self.state.lock().unwrap().container_token.clone()
    }

    /// Require IMDSv2 session tokens (default true).
    pub fn set_require_v2(&self, yes: bool) {
        self.state.lock().unwrap().require_v2 = yes;
    }

    /// Behave like an IMDSv1-only endpoint (`PUT /latest/api/token` answers 405, no tokens checked).
    pub fn set_v1_only(&self, yes: bool) {
        let mut s = self.state.lock().unwrap();
        s.v1_only = yes;
        s.require_v2 = !yes;
    }

    /// Number of credential documents served.
    pub fn credential_requests(&self) -> usize {
        self.state.lock().unwrap().credential_requests
    }

    /// Number of rejected requests (missing / bad tokens).
    pub fn rejected(&self) -> usize {
        self.state.lock().unwrap().rejected
    }
}

fn creds(s: &State) -> Value {
    obj([
        ("Code", Value::from("Success")),
        ("Type", Value::from("AWS-HMAC")),
        ("AccessKeyId", Value::from(s.access_key.clone())),
        ("SecretAccessKey", Value::from(s.secret_key.clone())),
        ("Token", Value::from(s.token.clone())),
        ("Expiration", Value::from(iso8601(now_secs() + 6 * 3600))),
        ("LastUpdated", Value::from(iso8601(now_secs()))),
    ])
}

fn handle(st: &Arc<Mutex<State>>, req: &crate::http::ServerRequest) -> crate::http::Response {
    use crate::http::Response;
    let mut s = st.lock().unwrap();
    if req.path == "/latest/api/token" {
        if s.v1_only {
            return Response::new(405);
        }
        if req.method != "PUT" || req.header("X-aws-ec2-metadata-token-ttl-seconds").is_none() {
            s.rejected += 1;
            return Response::new(400);
        }
        let t = format!("AQAEA{}", random_hex(16));
        s.tokens.push(t.clone());
        return Response::new(200).with_header("Content-Type", "text/plain").with_body(t.into_bytes());
    }
    if let Some(rest) = req.path.strip_prefix("/latest/meta-data/iam/security-credentials/") {
        let token_ok = req.header("X-aws-ec2-metadata-token").is_some_and(|t| s.tokens.iter().any(|x| x == t));
        if s.require_v2 && !token_ok {
            s.rejected += 1;
            return Response::new(401);
        }
        if rest.is_empty() {
            return Response::new(200).with_header("Content-Type", "text/plain").with_body(s.role.clone().into_bytes());
        }
        if rest != s.role {
            return Response::new(404);
        }
        s.credential_requests += 1;
        return Response::json(200, creds(&s).to_json());
    }
    if req.path.starts_with("/v2/credentials/") {
        if req.header("Authorization") != Some(s.container_token.as_str()) {
            s.rejected += 1;
            return Response::json(401, obj([("message", Value::from("Unauthorized"))]).to_json());
        }
        s.credential_requests += 1;
        return Response::json(200, creds(&s).to_json());
    }
    Response::new(404)
}

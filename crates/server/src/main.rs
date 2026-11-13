//! keyko-server: hands each caller the level keys its Tailscale grants allow, for one project
//! at a time. It trusts the identity headers `tailscale serve` adds, so it must only be reachable
//! through `tailscale serve`: it listens on loopback and refuses anything else unless told to.

// Handlers return a ready response as their error; its size does not matter at this traffic.
#![allow(clippy::result_large_err)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::SystemTime;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use keyko_core::{
    AccessResponse, DEFAULT_CAPABILITY, ErrorResponse, KeysResponse, OLD_CAPABILITY,
    parse_key_env_var, public_key_of, valid_name,
};
use serde::Deserialize;

const LOGIN_HEADER: &str = "tailscale-user-login";
const CAPS_HEADER: &str = "tailscale-app-capabilities";

/// What the server knows: keys per project and level, and how to read grants.
pub struct Server {
    /// project -> level -> private keys, newest first, comma-joined as clients receive them.
    keys: BTreeMap<String, BTreeMap<String, String>>,
    /// The default one also reads grants of the capability from before the rename.
    capabilities: Vec<String>,
    /// Fallback grants by login, for tailnets that cannot forward app capabilities.
    users: BTreeMap<String, Grants>,
}

/// project -> levels; `*` stands for every project or every level.
type Grants = BTreeMap<String, Vec<String>>;

#[derive(Deserialize)]
struct CapValue {
    #[serde(default)]
    projects: Grants,
}

#[derive(Deserialize)]
struct AccessFile {
    #[serde(default)]
    users: BTreeMap<String, Grants>,
}

impl Server {
    /// Reads `KEYKO_KEY__<PROJECT>__<LEVEL>` variables. Each holds one or more private keys,
    /// newest first, separated by commas or whitespace.
    pub fn from_env(
        vars: impl Iterator<Item = (String, String)>,
        capability: String,
        users: BTreeMap<String, Grants>,
    ) -> Result<Server, String> {
        let mut keys: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for (var, value) in vars {
            let Some((project, level)) = parse_key_env_var(&var) else {
                if var.starts_with("KEYKO_KEY_") {
                    return Err(format!("{var} is not KEYKO_KEY__<PROJECT>__<LEVEL>"));
                }
                continue;
            };
            let list: Vec<&str> = value
                .split([',', ' ', '\n', '\t', '\r'])
                .filter(|k| !k.is_empty())
                .collect();
            if list.is_empty() {
                return Err(format!("{var} is empty"));
            }
            let mut publics = Vec::new();
            for key in &list {
                match public_key_of(key) {
                    Some(public) => publics.push(public),
                    None => {
                        return Err(format!(
                            "{var} holds something that is not an age private key"
                        ));
                    }
                }
            }
            log(&format!("loaded {project}/{level}: {}", publics.join(", ")));
            keys.entry(project)
                .or_default()
                .insert(level, list.join(","));
        }
        let mut capabilities = vec![capability];
        if capabilities[0] == DEFAULT_CAPABILITY {
            capabilities.push(OLD_CAPABILITY.to_string());
        }
        Ok(Server {
            keys,
            capabilities,
            users,
        })
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/healthz", get(|| async { "ok" }))
            .route("/v1/access/{project}", get(access))
            .route("/v1/keys/{project}", get(keys))
            .with_state(Arc::new(self))
    }

    /// The caller's login and the levels of `project` it may have (that the server has keys for).
    fn granted(
        &self,
        headers: &HeaderMap,
        project: &str,
    ) -> Result<(String, BTreeSet<String>), Response> {
        let login = headers
            .get(LOGIN_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let caps = headers.get(CAPS_HEADER).and_then(|v| v.to_str().ok());
        if login.is_none() && caps.is_none() {
            return Err(error(
                StatusCode::UNAUTHORIZED,
                "no Tailscale identity: reach this server through tailscale serve",
            ));
        }
        let mut grants: Vec<Grants> = Vec::new();
        if let Some(caps) = caps {
            let parsed: BTreeMap<String, Vec<serde_json::Value>> = serde_json::from_str(caps)
                .map_err(|_| {
                    error(
                        StatusCode::BAD_REQUEST,
                        "unreadable Tailscale-App-Capabilities",
                    )
                })?;
            for capability in &self.capabilities {
                for value in parsed.get(capability).into_iter().flatten() {
                    match serde_json::from_value::<CapValue>(value.clone()) {
                        Ok(v) => grants.push(v.projects),
                        Err(_) => log(&format!("ignoring a malformed {capability} grant")),
                    }
                }
            }
        }
        if let Some(user) = login.as_ref().and_then(|l| self.users.get(l)) {
            grants.push(user.clone());
        }
        let available = self.keys.get(project);
        let mut levels = BTreeSet::new();
        for grant in grants {
            for (p, ls) in grant {
                if p != project && p != "*" {
                    continue;
                }
                for l in ls {
                    match available {
                        Some(available) if l == "*" => levels.extend(available.keys().cloned()),
                        Some(available) if available.contains_key(&l) => {
                            levels.insert(l);
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok((login.unwrap_or_else(|| "tagged-node".into()), levels))
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.to_string(),
        }),
    )
        .into_response()
}

fn log(message: &str) {
    println!(
        "{} {message}",
        humantime::format_rfc3339_seconds(SystemTime::now())
    );
}

fn check_project(project: &str) -> Result<(), Response> {
    if valid_name(project) {
        Ok(())
    } else {
        Err(error(StatusCode::BAD_REQUEST, "invalid project name"))
    }
}

async fn access(
    State(server): State<Arc<Server>>,
    Path(project): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_project(&project) {
        return r;
    }
    match server.granted(&headers, &project) {
        Err(r) => r,
        Ok((login, levels)) => {
            log(&format!(
                "access login={login} project={project} levels={}",
                join(&levels)
            ));
            Json(AccessResponse {
                project,
                login,
                levels: levels.into_iter().collect(),
            })
            .into_response()
        }
    }
}

#[derive(Deserialize)]
struct KeysQuery {
    levels: String,
}

async fn keys(
    State(server): State<Arc<Server>>,
    Path(project): Path<String>,
    Query(query): Query<KeysQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_project(&project) {
        return r;
    }
    let requested: BTreeSet<String> = query
        .levels
        .split(',')
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if requested.is_empty() || !requested.iter().all(|l| valid_name(l)) {
        return error(
            StatusCode::BAD_REQUEST,
            "levels must be a comma-separated list of level names",
        );
    }
    let (login, granted) = match server.granted(&headers, &project) {
        Err(r) => return r,
        Ok(g) => g,
    };
    let given: BTreeSet<String> = requested.intersection(&granted).cloned().collect();
    let denied: BTreeSet<String> = requested.difference(&granted).cloned().collect();
    // Who fetched which levels, and when; never a key.
    log(&format!(
        "fetch login={login} project={project} given={} denied={}",
        join(&given),
        join(&denied)
    ));
    if given.is_empty() {
        return error(
            StatusCode::FORBIDDEN,
            &format!("{login} may not have level {} of {project}", join(&denied)),
        );
    }
    let keys = server.keys[&project]
        .iter()
        .filter(|(l, _)| given.contains(*l))
        .map(|(l, k)| (l.clone(), k.clone()))
        .collect();
    Json(KeysResponse {
        project,
        login,
        keys,
    })
    .into_response()
}

fn join(set: &BTreeSet<String>) -> String {
    if set.is_empty() {
        "-".into()
    } else {
        set.iter().cloned().collect::<Vec<_>>().join(",")
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn main() {
    // SAFETY: before the runtime starts, so no other thread exists.
    for (old, new) in unsafe { keyko_core::adopt_old_env_names() } {
        log(&format!("{old} is now {new}; rename it"));
    }
    run();
}

#[tokio::main]
async fn run() {
    if let Err(e) = serve().await {
        eprintln!("keyko-server: {e}");
        std::process::exit(1);
    }
}

async fn serve() -> Result<(), String> {
    let listen: SocketAddr = env("KEYKO_LISTEN")
        .unwrap_or_else(|| "127.0.0.1:8787".into())
        .parse()
        .map_err(|e| format!("KEYKO_LISTEN: {e}"))?;
    if !listen.ip().is_loopback() && env("KEYKO_ALLOW_NON_LOOPBACK").as_deref() != Some("1") {
        return Err(format!(
            "refusing to listen on {listen}: anyone who reaches it could claim any identity. Listen on loopback behind tailscale serve, or set KEYKO_ALLOW_NON_LOOPBACK=1 if only tailscale serve can reach it"
        ));
    }
    let capability = env("KEYKO_CAPABILITY").unwrap_or_else(|| DEFAULT_CAPABILITY.into());
    let users = match env("KEYKO_ACCESS_FILE") {
        None => BTreeMap::new(),
        Some(path) => {
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
            toml::from_str::<AccessFile>(&text)
                .map_err(|e| format!("{path}: {e}"))?
                .users
        }
    };
    let server = Server::from_env(std::env::vars(), capability.clone(), users)?;
    if server.keys.is_empty() {
        log("warning: no KEYKO_KEY__<PROJECT>__<LEVEL> variables; every request will be refused");
    }
    log(&format!(
        "listening on {listen}, reading grants of {capability}"
    ));
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    axum::serve(listener, server.router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const CAP: &str = "example.com/cap/keyko";

    struct Fixture {
        router: Router,
        dev: String,
        old_dev: String,
        prod: String,
    }

    fn fixture() -> Fixture {
        let (dev, _) = keyko_core::generate_key();
        let (old_dev, _) = keyko_core::generate_key();
        let (prod, _) = keyko_core::generate_key();
        let (other, _) = keyko_core::generate_key();
        let vars = vec![
            (
                "KEYKO_KEY__MYAPP__DEV".to_string(),
                format!("{dev}, {old_dev}"),
            ),
            ("KEYKO_KEY__MYAPP__PRODUCTION".to_string(), prod.clone()),
            ("KEYKO_KEY__OTHER__DEV".to_string(), other),
            ("PATH".to_string(), "/bin".to_string()),
        ];
        let users = BTreeMap::from([(
            "bob@example.com".to_string(),
            Grants::from([("myapp".to_string(), vec!["production".to_string()])]),
        )]);
        let router = Server::from_env(vars.into_iter(), CAP.into(), users)
            .unwrap()
            .router();
        Fixture {
            router,
            dev,
            old_dev,
            prod,
        }
    }

    async fn call(
        router: &Router,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::get(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = router
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        )
    }

    fn caps(grants: &str) -> String {
        format!(r#"{{"{CAP}":[{grants}],"other.com/cap/x":[{{}}]}}"#)
    }

    #[tokio::test]
    async fn gives_only_granted_levels() {
        let f = fixture();
        let caps = caps(r#"{"projects":{"myapp":["dev"]}}"#);
        let h = [
            ("Tailscale-User-Login", "alice@example.com"),
            ("Tailscale-App-Capabilities", caps.as_str()),
        ];
        let (status, body) = call(&f.router, "/v1/keys/myapp?levels=dev,production", &h).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["login"], "alice@example.com");
        assert_eq!(body["keys"]["dev"], format!("{},{}", f.dev, f.old_dev));
        assert!(body["keys"].get("production").is_none());
        let (status, body) = call(&f.router, "/v1/keys/myapp?levels=production", &h).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["error"].as_str().unwrap().contains("production"));
        let (status, body) = call(&f.router, "/v1/access/myapp", &h).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["levels"], serde_json::json!(["dev"]));
    }

    #[tokio::test]
    async fn reads_grants_of_the_capability_from_before_the_rename() {
        let (dev, _) = keyko_core::generate_key();
        let vars = || vec![("KEYKO_KEY__MYAPP__DEV".to_string(), dev.clone())].into_iter();
        let caps = format!(r#"{{"{OLD_CAPABILITY}":[{{"projects":{{"myapp":["dev"]}}}}]}}"#);
        let h = [("Tailscale-App-Capabilities", caps.as_str())];
        let default = Server::from_env(vars(), DEFAULT_CAPABILITY.into(), BTreeMap::new());
        let (_, body) = call(&default.unwrap().router(), "/v1/access/myapp", &h).await;
        assert_eq!(body["levels"], serde_json::json!(["dev"]));
        // A capability set by hand is the only one read.
        let custom = Server::from_env(vars(), CAP.into(), BTreeMap::new());
        let (_, body) = call(&custom.unwrap().router(), "/v1/access/myapp", &h).await;
        assert_eq!(body["levels"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn merges_grants_and_wildcards() {
        let f = fixture();
        let caps = caps(r#"{"projects":{"other":["dev"]}},{"projects":{"*":["*"]}}"#);
        let h = [("Tailscale-App-Capabilities", caps.as_str())];
        let (status, body) = call(&f.router, "/v1/keys/myapp?levels=dev,production", &h).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["login"], "tagged-node");
        assert_eq!(body["keys"]["production"], f.prod);
        let (_, body) = call(&f.router, "/v1/access/nothing", &h).await;
        assert_eq!(body["levels"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn falls_back_to_the_access_file_by_login() {
        let f = fixture();
        let h = [("Tailscale-User-Login", "bob@example.com")];
        let (status, body) = call(&f.router, "/v1/keys/myapp?levels=production", &h).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["keys"]["production"], f.prod);
        let h = [("Tailscale-User-Login", "eve@example.com")];
        let (status, _) = call(&f.router, "/v1/keys/myapp?levels=dev", &h).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn refuses_requests_without_tailscale_identity_or_with_bad_input() {
        let f = fixture();
        let (status, _) = call(&f.router, "/v1/keys/myapp?levels=dev", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let h = [
            ("Tailscale-User-Login", "alice@example.com"),
            ("Tailscale-App-Capabilities", "nope"),
        ];
        let (status, _) = call(&f.router, "/v1/keys/myapp?levels=dev", &h).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let h = [("Tailscale-User-Login", "alice@example.com")];
        let (status, _) = call(&f.router, "/v1/keys/Mine?levels=dev", &h).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&f.router, "/v1/keys/myapp?levels=../x", &h).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&f.router, "/healthz", &[]).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn rejects_bad_key_variables() {
        let bad = [
            ("KEYKO_KEY__MYAPP__DEV", "not-a-key"),
            ("KEYKO_KEY__MYAPP__DEV", " "),
            ("KEYKO_KEY_X", "x"),
        ];
        for (var, value) in bad {
            let vars = vec![(var.to_string(), value.to_string())];
            let err = Server::from_env(vars.into_iter(), CAP.into(), BTreeMap::new())
                .err()
                .unwrap();
            assert!(
                !err.contains(value.trim()) || value.trim().is_empty() || value == "x",
                "{err}"
            );
        }
    }
}

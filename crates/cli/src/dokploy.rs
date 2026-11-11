//! The bits of the Dokploy API litekeys uses: read an application's or compose service's
//! environment, save it, deploy it. The API key travels in a header, never in a process list.

use serde_json::{Value, json};

use crate::config::AppKind;
use crate::{Result, fail};

pub struct Dokploy {
    url: String,
    key: String,
    agent: ureq::Agent,
}

impl Dokploy {
    pub fn new(url: &str, key: String) -> Dokploy {
        Dokploy {
            url: url.trim_end_matches('/').to_string(),
            key,
            agent: crate::http::agent(),
        }
    }

    fn get(&self, path: &str, query: (&str, &str)) -> Result<Value> {
        let url = format!("{}/api/{path}", self.url);
        let mut response = self
            .agent
            .get(&url)
            .query(query.0, query.1)
            .header("x-api-key", &self.key)
            .header("accept", "application/json")
            .call()
            .map_err(|e| format!("cannot reach Dokploy at {}: {e}", self.url))?;
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("Dokploy: {e}"))?;
        if response.status() != 200 {
            return fail(format!(
                "Dokploy answered {} to {path}: {}",
                response.status(),
                crate::http::error_text(&body)
            ));
        }
        serde_json::from_str(&body).map_err(|e| format!("Dokploy sent an unexpected answer: {e}"))
    }

    fn post(&self, path: &str, body: &Value) -> Result<()> {
        let url = format!("{}/api/{path}", self.url);
        let mut response = self
            .agent
            .post(&url)
            .header("x-api-key", &self.key)
            .header("content-type", "application/json")
            .send(body.to_string())
            .map_err(|e| format!("cannot reach Dokploy at {}: {e}", self.url))?;
        if !(200..300).contains(&response.status().as_u16()) {
            let text = response.body_mut().read_to_string().unwrap_or_default();
            return fail(format!(
                "Dokploy refused {path} ({}): {}",
                response.status(),
                crate::http::error_text(&text)
            ));
        }
        Ok(())
    }

    pub fn app(&self, kind: AppKind, id: &str) -> Result<App> {
        let raw = match kind {
            AppKind::Application => self.get("application.one", ("applicationId", id))?,
            AppKind::Compose => self.get("compose.one", ("composeId", id))?,
        };
        Ok(App {
            kind,
            id: id.to_string(),
            raw,
        })
    }

    /// Replaces the environment, keeping an application's build settings.
    pub fn save_env(&self, app: &App, env: &str) -> Result<()> {
        match app.kind {
            AppKind::Application => self.post(
                "application.saveEnvironment",
                &json!({
                    "applicationId": app.id,
                    "env": env,
                    "buildArgs": app.raw.get("buildArgs"),
                    "buildSecrets": app.raw.get("buildSecrets"),
                    "createEnvFile": app.raw.get("createEnvFile"),
                }),
            ),
            AppKind::Compose => self.post(
                "compose.update",
                &json!({ "composeId": app.id, "env": env }),
            ),
        }
    }

    pub fn deploy(&self, app: &App) -> Result<()> {
        match app.kind {
            AppKind::Application => {
                self.post("application.deploy", &json!({ "applicationId": app.id }))
            }
            AppKind::Compose => self.post("compose.deploy", &json!({ "composeId": app.id })),
        }
    }
}

pub struct App {
    pub kind: AppKind,
    pub id: String,
    raw: Value,
}

impl App {
    pub fn name(&self) -> String {
        self.raw
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&self.id)
            .to_string()
    }

    pub fn env(&self) -> String {
        self.raw
            .get("env")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }
}

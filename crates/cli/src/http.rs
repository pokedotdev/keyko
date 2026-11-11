//! One HTTP client for the key server, Dokploy and the sops download.

use std::net::SocketAddr;
use std::time::Duration;

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// Tailscale's MagicDNS resolver, reachable from any machine on the tailnet.
const MAGIC_DNS: &str = "100.100.100.100:53";

/// HTTP statuses come back as responses, not errors, so callers can read error bodies.
pub fn agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .user_agent(concat!("tierkey/", env!("CARGO_PKG_VERSION")))
        .build();
    ureq::Agent::with_parts(config, DefaultConnector::new(), TailnetResolver)
}

/// The system resolver, then MagicDNS for `*.ts.net` names the system resolver cannot find.
#[derive(Debug)]
struct TailnetResolver;

/// The MagicDNS resolver: TIERKEY_MAGIC_DNS (`host:port`, for tests), else 100.100.100.100:53.
fn magic_dns() -> String {
    std::env::var("TIERKEY_MAGIC_DNS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| MAGIC_DNS.to_string())
}

pub fn is_tailnet_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host.ends_with(".ts.net")
}

impl Resolver for TailnetResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let system = match DefaultResolver::default().resolve(uri, config, timeout) {
            Ok(addrs) => return Ok(addrs),
            Err(e) => e,
        };
        let Some(host) = uri.host().filter(|h| is_tailnet_host(h)) else {
            return Err(system);
        };
        let port = uri
            .port_u16()
            .unwrap_or(if uri.scheme_str() == Some("http") {
                80
            } else {
                443
            });
        let server = magic_dns();
        let found = server
            .parse::<SocketAddr>()
            .map_err(|e| std::io::Error::other(format!("bad address {server}: {e}")))
            .and_then(|s| crate::dns::lookup(s, host, Duration::from_secs(3)));
        match found {
            Ok(ips) if !ips.is_empty() => {
                // ResolvedSocketAddrs holds at most 16.
                let mut addrs = self.empty();
                for ip in ips.into_iter().take(16) {
                    addrs.push(SocketAddr::new(ip, port));
                }
                Ok(addrs)
            }
            Ok(_) => Err(not_found(host, &system, &server, "no such name")),
            Err(e) => Err(not_found(host, &system, &server, &e.to_string())),
        }
    }
}

fn not_found(host: &str, system: &ureq::Error, server: &str, magic: &str) -> ureq::Error {
    ureq::Error::Io(std::io::Error::other(format!(
        "cannot resolve {host}: the system resolver says {system}, and Tailscale's MagicDNS \
         at {server} says {magic}"
    )))
}

/// The `error` field of a JSON error body, or the body itself.
pub fn error_text(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.trim().chars().take(200).collect())
}

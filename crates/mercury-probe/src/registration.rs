//! Registration with the core: on start and every 60 s, the probe tells its core what it serves.

use std::time::Duration;

use mercury_schema::api::ProbeRegistration;
use mercury_schema::facts::SchemaId;
use tokio::sync::watch;

use crate::probe::Probe;
use crate::tools::served;

pub const REGISTER_EVERY: Duration = Duration::from_secs(60);

/// The registration body: the mode, the manifest and its hash of the probe the index was built from.
pub fn body(probe: &Probe) -> ProbeRegistration {
    ProbeRegistration {
        project: probe.manifest.project.clone(),
        schema: SchemaId,
        access: probe.access,
        access_limited_by: probe.access_limited_by,
        manifest_hash: probe.manifest_hash.clone(),
        manifest: probe.manifest.clone(),
        tools: served(probe.access).to_vec(),
    }
}

/// A body that never changes, for a caller with nothing to rebuild.
pub fn fixed(body: ProbeRegistration) -> watch::Receiver<ProbeRegistration> {
    watch::channel(body).1
}

/// Sends the latest body now and then every `every`, until the task is dropped; a rebuild that
/// moves the mode or the manifest sends a new body down `registration`, and the next tick carries
/// it. A refused or failed registration is logged by its status only, and the next tick tries
/// again.
pub async fn keep_registering(
    core: String,
    token: Option<String>,
    registration: watch::Receiver<ProbeRegistration>,
    every: Duration,
) {
    let url = format!("{}/internal/probes/register", core.trim_end_matches('/'));
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(5)).build() {
        Ok(client) => client,
        Err(_) => {
            eprintln!("mercury-probe: registration can't start: no HTTP client");
            return;
        }
    };
    let mut ticks = tokio::time::interval(every);
    let mut last_ok = None;
    loop {
        ticks.tick().await;
        let body = registration.borrow().clone();
        let mut request = client.post(&url).json(&body);
        if let Some(token) = &token {
            request = request.bearer_auth(token);
        }
        let ok = match request.send().await {
            Ok(response) if response.status().is_success() => true,
            Ok(response) => {
                eprintln!(
                    "mercury-probe: the core refused the registration ({})",
                    response.status().as_u16()
                );
                false
            }
            Err(err) if err.is_timeout() => {
                eprintln!("mercury-probe: the registration timed out");
                false
            }
            Err(_) => {
                eprintln!("mercury-probe: the core can't be reached for registration");
                false
            }
        };
        if ok && last_ok != Some(true) {
            eprintln!("mercury-probe: registered with the core");
        }
        last_ok = Some(ok);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mercury_schema::{AccessMode, ToolName};

    use super::*;

    #[test]
    fn registration_names_ask_code_only_under_code_model() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta");
        let mut probe = Probe::dev(&repo, &repo.join("meta"), None).unwrap();
        for (access, tools) in [
            (AccessMode::Metadata, &ToolName::ALL[..]),
            (AccessMode::Code, &ToolName::ALL[..]),
            (AccessMode::CodeModel, &ToolName::CODE_MODEL[..]),
        ] {
            probe.access = access;
            let registered = body(&probe).tools;
            assert_eq!(registered, tools, "{access:?}");
            assert_eq!(
                registered.contains(&ToolName::AskCode),
                access == AccessMode::CodeModel
            );
        }
    }
}

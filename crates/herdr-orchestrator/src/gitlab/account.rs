//! Which token a GitLab host is read with, where it came from, and whom
//! GitLab takes it for: what Settings > GitLab shows for each host.

use super::{Api, Error, Scheme, TIMEOUT, authorization, cli_token, environment_token, saved};
use herdr_credentials::Store;
use secrecy::SecretString;
use serde::Deserialize;
use std::{ffi::OsString, io::Read};

/// The answer to `/user` is a small object; anything larger is refused.
const LIMIT: u64 = 64 * 1024;

/// Where a host's token came from, in the order they are tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `GITLAB_TOKEN` or `GITLAB_ACCESS_TOKEN`, for the host `GITLAB_HOST` names.
    Environment,
    /// Pasted in Settings > GitLab.
    Saved,
    /// The GitLab CLI's own sign-in (`glab auth login`).
    Cli,
}

/// A host's token and where it came from.
pub struct Found {
    pub token: SecretString,
    pub source: Source,
}

/// The token to read `host` as: the environment's when it is meant for this
/// host, else the one saved in `store`, else the GitLab CLI's. A store that
/// cannot be read is skipped, so the CLI's sign-in still serves. Blocks on
/// the store and a subprocess: call it from a worker.
pub fn lookup(host: &str, store: Store) -> Option<Found> {
    resolve(
        host,
        |name| std::env::var_os(name),
        || match saved::read(store) {
            Ok(saved) => saved.get(host).cloned(),
            Err(error) => {
                tracing::warn!(
                    category = "orchestrator_gitlab",
                    %error,
                    "Saved GitLab tokens could not be read"
                );
                None
            }
        },
        || cli_token(host),
    )
}

/// [`lookup`] with the host's saved token already read, so a caller listing
/// many hosts reads the store once. Blocks on a subprocess.
pub fn lookup_with(host: &str, saved: Option<SecretString>) -> Option<Found> {
    resolve(
        host,
        |name| std::env::var_os(name),
        || saved,
        || cli_token(host),
    )
}

/// [`lookup`]'s order over the given environment, saved token, and CLI;
/// a later one is asked only when every earlier one has no token.
pub fn resolve(
    host: &str,
    variable: impl Fn(&str) -> Option<OsString>,
    saved: impl FnOnce() -> Option<SecretString>,
    cli: impl FnOnce() -> Option<SecretString>,
) -> Option<Found> {
    let found = |source| move |token| Found { token, source };
    environment_token(host, variable)
        .map(found(Source::Environment))
        .or_else(|| saved().map(found(Source::Saved)))
        .or_else(|| cli().map(found(Source::Cli)))
}

#[derive(Deserialize)]
struct User {
    username: String,
}

/// The username GitLab at `api` signs `token` in as. Always over HTTPS, so
/// the token is never sent in the clear: an API only served over HTTP is
/// refused unasked. Blocks on the network.
pub fn whoami(api: &Api, token: &SecretString) -> Result<String, Error> {
    if api.scheme == Scheme::Http {
        return Err(Error::InsecureHost(api.instance.clone()));
    }
    let origin = format!("https://{}", api.host);
    let url = url::Url::parse(&format!("{origin}/api/v4/user"))
        .map_err(|_| Error::Address(api.host.clone()))?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let mut reply = agent
        .get(url.as_str())
        .header("User-Agent", "Herdr-GPUI")
        .header("Accept", "application/json")
        .header("Authorization", authorization(token)?)
        .call()
        .map_err(|error| Error::Network {
            origin,
            source: Box::new(error),
        })?;
    match reply.status().as_u16() {
        200 => {}
        401 => return Err(Error::Authentication),
        403 => return Err(Error::Scope),
        429 => return Err(Error::RateLimit),
        status => return Err(Error::Status(status)),
    }
    let mut bytes = Vec::new();
    reply
        .body_mut()
        .as_reader()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::Read)?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::Size);
    }
    let user: User = serde_json::from_slice(&bytes).map_err(Error::Json)?;
    Ok(user.username)
}

//! Which token a GitLab instance is read with, where it came from, and whom
//! GitLab takes it for: what the title bar's account menu shows for each.

use super::{
    Api, Error, Scheme, TIMEOUT, authorization, cli_token, environment_token, envrc, saved,
};
use herdr_credentials::Store;
use secrecy::SecretString;
use serde::Deserialize;
use std::{ffi::OsString, io::Read, path::Path};

/// The answer to `/user` is a small object; anything larger is refused.
const LIMIT: u64 = 64 * 1024;

/// Where a token came from, in the order they are tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `GITLAB_TOKEN` or `GITLAB_ACCESS_TOKEN` that the checkout's `.envrc`
    /// sets, as direnv loads it there.
    Envrc,
    /// `GITLAB_TOKEN` or `GITLAB_ACCESS_TOKEN` in the login shell, for the
    /// instance `GITLAB_HOST` names.
    Environment,
    /// Pasted in the account menu.
    Saved,
    /// The GitLab CLI's own sign-in (`glab auth login`).
    Cli,
}

/// A token and where it came from.
pub struct Found {
    pub token: SecretString,
    pub source: Source,
}

/// The token to read instance `host` as: the one `checkout`'s `.envrc` sets,
/// then the login shell's when it is meant for this instance, then the one
/// saved in `store`, then the GitLab CLI's. `checkout` is a folder on this
/// computer, or `None` for a remote one, whose `.envrc` is not run here. A
/// store that cannot be read is skipped, so the CLI's sign-in still serves.
/// Blocks on direnv, the store, and the CLI: call it from a worker.
pub fn lookup(host: &str, store: Store, checkout: Option<&Path>) -> Option<Found> {
    let saved = || match saved::read(store) {
        Ok(saved) => saved.get(host).cloned(),
        Err(error) => {
            tracing::warn!(
                category = "orchestrator_gitlab",
                %error,
                "Saved GitLab tokens could not be read"
            );
            None
        }
    };
    resolve(
        host,
        || checkout.and_then(|checkout| envrc::token(checkout, host)),
        herdr_client::login_env::var,
        saved,
        || cli_token(host),
    )
}

/// [`lookup`] with the instance's saved token already read, so a caller
/// listing many instances reads the store once. Blocks on direnv and the CLI.
pub fn lookup_with(
    host: &str,
    saved: Option<SecretString>,
    checkout: Option<&Path>,
) -> Option<Found> {
    resolve(
        host,
        || checkout.and_then(|checkout| envrc::token(checkout, host)),
        herdr_client::login_env::var,
        || saved,
        || cli_token(host),
    )
}

/// [`lookup`]'s order over the given `.envrc` token, environment, saved
/// token, and CLI; a later one is asked only when every earlier one has none.
pub fn resolve(
    host: &str,
    envrc: impl FnOnce() -> Option<SecretString>,
    variable: impl Fn(&str) -> Option<OsString>,
    saved: impl FnOnce() -> Option<SecretString>,
    cli: impl FnOnce() -> Option<SecretString>,
) -> Option<Found> {
    let found = |source| move |token| Found { token, source };
    envrc()
        .map(found(Source::Envrc))
        .or_else(|| environment_token(host, variable).map(found(Source::Environment)))
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

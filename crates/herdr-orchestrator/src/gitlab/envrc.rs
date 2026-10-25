//! A checkout's own GitLab token: the `GITLAB_TOKEN` its `.envrc` sets, as
//! direnv loads it there. direnv runs only an `.envrc` the user allowed with
//! `direnv allow`, so this runs nothing the user's own shell would not.

use super::{CLI_TIMEOUT, TOKEN_VARIABLES, environment_token};
use secrecy::{ExposeSecret, SecretString};
use std::{collections::HashMap, ffi::OsString, path::Path, time::Instant};

/// The whole environment `env` prints; the rest of a large one is dropped.
const OUTPUT_LIMIT: usize = 256 * 1024;

/// Whether direnv has a file to load for `checkout`: an `.envrc` there or in
/// a folder above it.
fn has_envrc(checkout: &Path) -> bool {
    checkout
        .ancestors()
        .any(|folder| folder.join(".envrc").is_file())
}

/// The token `checkout`'s `.envrc` sets for instance `host`, if direnv is
/// installed and the file is allowed. Blocks on a subprocess: call it from
/// a worker.
pub fn token(checkout: &Path, host: &str) -> Option<SecretString> {
    if !has_envrc(checkout) {
        return None;
    }
    let mut command = herdr_client::new_command("direnv");
    // An app opened from a desktop launcher has a minimal PATH.
    herdr_client::login_env::apply(&mut command);
    command
        .env("DIRENV_LOG_FORMAT", "")
        .arg("exec")
        .arg(checkout)
        .arg("env");
    let result = herdr_github::pull_request::run_bytes(
        &mut command,
        Instant::now() + CLI_TIMEOUT,
        &|| false,
        OUTPUT_LIMIT,
    );
    match result {
        Ok((true, output)) => from_env(&output, host, herdr_client::login_env::var),
        Ok((false, _)) => None,
        Err(error) => {
            tracing::debug!(
                category = "orchestrator_gitlab",
                error = %error,
                "direnv could not be asked for a checkout's GitLab token"
            );
            None
        }
    }
}

/// The token in `env`'s output for `host`, unless the login shell (`shell`)
/// has the same one: that one is the environment's, not the checkout's.
pub fn from_env(
    output: &[u8],
    host: &str,
    shell: impl Fn(&str) -> Option<OsString>,
) -> Option<SecretString> {
    let text = String::from_utf8_lossy(output);
    let set: HashMap<&str, &str> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| *name == "GITLAB_HOST" || TOKEN_VARIABLES.contains(name))
        .collect();
    let token = environment_token(host, |name| set.get(name).map(OsString::from))?;
    let own = environment_token(host, shell);
    own.is_none_or(|own| own.expose_secret() != token.expose_secret())
        .then_some(token)
}

//! Tokens pasted in Settings > GitLab, one per host, kept together in one
//! credential-store record of `host<TAB>token` lines: the desktop keyring, or
//! the private `gitlab-credentials` file when `[gitlab]
//! allow_plaintext_credentials` opts in. Every read and write blocks on the
//! store, so callers run on a worker.

use super::{Error, valid};
use herdr_credentials::{Entry, Store};
use secrecy::{ExposeSecret, SecretString};
use std::{collections::BTreeMap, path::PathBuf};
use zeroize::Zeroizing;

/// The most hosts one record keeps, which also bounds its size.
pub const MAX_HOSTS: usize = 16;
/// A host name with an optional port, as a remote names it.
const HOST_LIMIT: usize = 260;

const ENTRY: Entry<Error> = Entry {
    service: "dev.herdr.gpui.gitlab",
    account: "tokens",
    label: "Herdr UI GitLab tokens",
    file: c"gitlab-credentials",
    validate: |record| Saved::parse(record).map(drop),
};

/// The saved tokens, by lowercase host.
#[derive(Default)]
pub struct Saved(BTreeMap<String, SecretString>);

impl Saved {
    pub fn get(&self, host: &str) -> Option<&SecretString> {
        self.0.get(&host.to_ascii_lowercase())
    }

    /// The hosts a token is saved for, sorted.
    pub fn hosts(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Keeps `token` for `host`, or forgets the host's token with `None`.
    pub fn set(&mut self, host: &str, token: Option<SecretString>) -> Result<(), Error> {
        let host = normalize_host(host).ok_or_else(|| Error::Host(host.to_owned()))?;
        match token {
            Some(token) => {
                let token = valid(token.expose_secret().trim()).ok_or(Error::Token)?;
                if !self.0.contains_key(&host) && self.0.len() >= MAX_HOSTS {
                    return Err(Error::TooManyHosts);
                }
                self.0.insert(host, token);
            }
            None => {
                self.0.remove(&host);
            }
        }
        Ok(())
    }

    /// Reads a record, refusing one that is not lines of a valid host and
    /// token or that names more than [`MAX_HOSTS`] hosts.
    pub fn parse(record: &SecretString) -> Result<Self, Error> {
        let mut saved = BTreeMap::new();
        for line in record
            .expose_secret()
            .lines()
            .filter(|line| !line.is_empty())
        {
            let (host, token) = line.split_once('\t').ok_or(Error::Record)?;
            let host = normalize_host(host).filter(|normal| normal == host);
            let (Some(host), Some(token)) = (host, valid(token)) else {
                return Err(Error::Record);
            };
            saved.insert(host, token);
        }
        if saved.len() > MAX_HOSTS {
            return Err(Error::Record);
        }
        Ok(Self(saved))
    }

    fn record(&self) -> SecretString {
        let mut text = Zeroizing::new(String::new());
        for (host, token) in &self.0 {
            text.push_str(host);
            text.push('\t');
            text.push_str(token.expose_secret());
            text.push('\n');
        }
        SecretString::from(text.as_str())
    }
}

/// A host as `Settings > GitLab` and remotes name it: lowercase, its port
/// kept, and any `https://` or trailing `/` a pasted address carries dropped.
pub fn normalize_host(input: &str) -> Option<String> {
    let host = super::configured_host(input).to_ascii_lowercase();
    let (name, port) = match host.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host.as_str(), None),
    };
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        && !name.starts_with(['.', '-']);
    let port_ok = port.is_none_or(|port| port.parse::<u16>().is_ok_and(|port| port > 0));
    (name_ok && port_ok && host.len() <= HOST_LIMIT).then_some(host)
}

/// The folder the GUI config, and so the private credential file, lives in.
fn directory() -> Result<PathBuf, Error> {
    Ok(herdr_client::local_path::config_root()
        .map_err(Error::Directory)?
        .join("herdr"))
}

/// Every saved token in `store`.
pub fn read(store: Store) -> Result<Saved, Error> {
    match herdr_credentials::read_entry(store, &ENTRY, &directory()?)? {
        Some(record) => Saved::parse(&record),
        None => Ok(Saved::default()),
    }
}

/// Saves `token` for `host` in `store`, or forgets it with `None`, keeping
/// the other hosts' tokens.
pub fn save(store: Store, host: &str, token: Option<SecretString>) -> Result<(), Error> {
    let mut saved = read(store)?;
    saved.set(host, token)?;
    let record = (!saved.0.is_empty()).then(|| saved.record());
    herdr_credentials::save_entry(store, &ENTRY, &directory()?, record.as_ref())
}

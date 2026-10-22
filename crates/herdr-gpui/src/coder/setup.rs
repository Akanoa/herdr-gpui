//! The blocking jobs behind Add Device's Coder form: sign in, list what the
//! account can use, create or attach a workspace, wait for it, check for (and
//! on approval install) Herdr, and save the device. Each runs on a worker.

use super::{
    Error, Result, Settings,
    api::{self, Client, Preset, Progress, Template, User, Workspace},
    connect, oauth, store,
};
use crate::cloud::{self, CloudProvider, SavedDevice, install};
use secrecy::SecretString;

pub(crate) use super::destroy::Stage;
pub(crate) use oauth::Pending;

/// What the account can use, shown once signed in.
#[derive(Clone, Debug)]
pub(crate) struct Account {
    pub(crate) user: User,
    pub(crate) templates: Vec<Template>,
    pub(crate) workspaces: Vec<Workspace>,
    /// IDs of this deployment's workspaces already saved as devices.
    pub(crate) saved: Vec<String>,
}

/// Where a new device comes from.
#[derive(Clone, Debug)]
pub(crate) enum Source {
    New {
        name: String,
        template: Template,
        preset: Option<Preset>,
    },
    Existing {
        id: String,
        name: String,
    },
}

/// A workspace whose agent accepts `coder ssh`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ready {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) agent: String,
}

/// A step of `provision`, for the dialog's status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Creating,
    Waiting(Progress),
    CheckingHerdr,
}

fn tokens(settings: &Settings) -> impl Fn(bool) -> Result<SecretString> + '_ {
    |rejected| store::current_token(settings, rejected)
}

/// Bind the redirect listener and build the page to open in the browser.
pub(crate) fn begin_sign_in(settings: &Settings) -> Result<Pending> {
    oauth::begin(settings)
}

/// Wait for the browser, save the sign-in, and list what the account can use.
pub(crate) fn finish_sign_in(
    settings: &Settings,
    pending: Pending,
    cancelled: impl Fn() -> bool,
) -> Result<Account> {
    pending.finish(settings, &cancelled, |credential| {
        store::save(settings, &credential)
    })?;
    account(settings)
}

pub(crate) fn signed_in(settings: &Settings) -> Result<bool> {
    store::signed_in(settings)
}

/// Forget the saved sign-in. The token is revoked at Coder first when possible;
/// a failed revocation never keeps a local sign-in the user asked to remove.
pub(crate) fn sign_out(settings: &Settings) -> Result<()> {
    if let Ok(token) = store::current_token(settings, false) {
        let mut url = url::Url::parse(&settings.endpoint("/oauth2/tokens"))
            .map_err(|_| Error::Url("coder.url"))?;
        url.query_pairs_mut()
            .append_pair("client_id", &settings.client_id);
        if let Err(error) = super::http::delete("oauth2_revoke", &token, url.as_str()) {
            tracing::warn!(category = "coder_signout", %error, "Could not revoke the Coder token");
        }
    }
    store::remove(settings)
}

pub(crate) fn account(settings: &Settings) -> Result<Account> {
    let saved = saved_devices(settings)?
        .into_iter()
        .map(|saved| saved.id)
        .collect();
    let tokens = tokens(settings);
    store::with_token(&tokens, |token| {
        let client = Client::new(settings, token);
        Ok(Account {
            user: client.me()?,
            templates: client.templates()?,
            workspaces: client.workspaces()?,
            saved: Vec::new(),
        })
    })
    .map(|account| Account { saved, ..account })
}

/// Forget a saved device and list the account again.
pub(crate) fn forget(settings: &Settings, id: &str) -> Result<Account> {
    forget_device(id)?;
    account(settings)
}

pub(crate) fn presets(settings: &Settings, template: &Template) -> Result<Vec<Preset>> {
    store::with_token(&tokens(settings), |token| {
        Client::new(settings, token).presets(template)
    })
}

fn ssh(settings: &Settings, ready: &Ready) -> Result<std::process::Command> {
    let token = store::current_token(settings, false)?;
    connect::ssh_command(
        &connect::cli(settings)?,
        settings,
        &token,
        &ready.name,
        &ready.agent,
    )
}

pub(crate) fn install(
    settings: &Settings,
    ready: &Ready,
    cancelled: impl Fn() -> bool,
) -> Result<()> {
    install::install(ssh(settings, ready)?, &cancelled)?;
    // The installer's own success is not proof the bridge will find it.
    if !install::installed(ssh(settings, ready)?, &cancelled)? {
        return Err(cloud::Error::Install(
            "the installer finished but Herdr is not on the expected paths".into(),
        )
        .into());
    }
    Ok(())
}

/// Save the device; the endpoint list picks it up on its next catalog read.
pub(crate) fn save(
    settings: &Settings,
    ready: &Ready,
    label: &str,
    session: &str,
) -> Result<SavedDevice> {
    let device = SavedDevice {
        provider: CloudProvider::Coder,
        id: ready.id.clone(),
        label: label.trim().to_owned(),
        account: settings.base.clone(),
        machine: ready.name.clone(),
        session: session.to_owned(),
        enabled: true,
    };
    cloud::save(device.clone())?;
    Ok(device)
}

/// This deployment's saved Coder devices.
fn saved_devices(settings: &Settings) -> Result<Vec<SavedDevice>> {
    Ok(cloud::load()?
        .into_iter()
        .filter(|saved| saved.provider == CloudProvider::Coder && saved.account == settings.base)
        .collect())
}

/// The session every Coder device attaches to.
const SESSION: &str = "default";

/// What adding a Coder device needs from the dialog.
pub(crate) struct AddRequest {
    pub(crate) settings: Settings,
    pub(crate) source: Source,
    pub(crate) label: String,
    /// The user approved running Herdr's installer if the workspace lacks it.
    pub(crate) install: bool,
}

fn step(step: Step) -> cloud::Step {
    match step {
        Step::Creating => cloud::Step::Creating,
        Step::Waiting(Progress::Starting) => cloud::Step::Starting,
        Step::Waiting(Progress::Building(status)) => {
            cloud::Step::Building(format!("{status:?}").to_lowercase())
        }
        Step::CheckingHerdr => cloud::Step::CheckingHerdr,
    }
}

/// The whole job for `cloud::Jobs`: create or attach, wait, install if
/// approved and needed, and save the device.
pub(crate) fn add_device(
    request: AddRequest,
    cancelled: &dyn Fn() -> bool,
    report: &dyn Fn(cloud::Step),
) -> crate::Result<SavedDevice> {
    let settings = &request.settings;
    // Fail before creating anything when the transport cannot run later.
    connect::cli(settings)?;
    let tokens = tokens(settings);
    let (id, name) = match &request.source {
        Source::Existing { id, name } => {
            return finish(
                &request,
                &tokens,
                id.clone(),
                name.clone(),
                cancelled,
                report,
            );
        }
        Source::New {
            name,
            template,
            preset,
        } => {
            report(step(Step::Creating));
            let created = store::with_token(&tokens, |token| {
                Client::new(settings, token).create(name, template, preset.as_ref())
            })?;
            (created.id, created.name)
        }
    };
    cloud::discard_on_error(
        CloudProvider::Coder,
        &id,
        || finish(&request, &tokens, id.clone(), name, cancelled, report),
        || {
            Ok(super::destroy::destroy(
                settings,
                &tokens,
                &id,
                || false,
                |_| {},
            )?)
        },
    )
}

/// Wait until workspace `id`'s agent is ready, check for (and when
/// `request` allows, install) Herdr, and save the device.
fn finish(
    request: &AddRequest,
    tokens: &impl Fn(bool) -> Result<SecretString>,
    id: String,
    name: String,
    cancelled: &dyn Fn() -> bool,
    report: &dyn Fn(cloud::Step),
) -> crate::Result<SavedDevice> {
    let settings = &request.settings;
    let (_, agent) = api::wait_ready(settings, tokens, &id, cancelled, |progress| {
        report(step(Step::Waiting(progress)));
    })?;
    let ready = Ready { id, name, agent };
    report(step(Step::CheckingHerdr));
    if !install::installed(ssh(settings, &ready)?, &cancelled)? {
        if !request.install {
            return Err(cloud::Error::Install(format!(
                "Herdr is not installed in {}; add it and try again",
                ready.name
            ))
            .into());
        }
        report(cloud::Step::InstallingHerdr);
        install(settings, &ready, cancelled)?;
    }
    let label = if request.label.is_empty() {
        &ready.name
    } else {
        &request.label
    };
    Ok(save(settings, &ready, label, SESSION)?)
}

/// The sign-in as Settings shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Session {
    SignedOut,
    SignedIn {
        user: String,
    },
    /// A sign-in is saved but the account could not be read; `reason` is why.
    Unreadable {
        reason: String,
    },
}

/// What the Settings section shows about a deployment.
#[derive(Clone, Debug)]
pub(crate) struct Overview {
    pub(crate) session: Session,
    /// Whether a client secret is saved from Settings.
    pub(crate) secret_saved: bool,
    /// This deployment's saved devices.
    pub(crate) devices: Vec<SavedDevice>,
}

pub(crate) fn overview(settings: &Settings) -> Result<Overview> {
    let devices = saved_devices(settings)?;
    let secret_saved = store::client_secret(settings)?.is_some();
    let session = if store::signed_in(settings)? {
        match store::with_token(&tokens(settings), |token| Client::new(settings, token).me()) {
            Ok(user) => Session::SignedIn {
                user: user.username,
            },
            Err(Error::Authentication) => Session::SignedOut,
            Err(error) => Session::Unreadable {
                reason: error.to_string(),
            },
        }
    } else {
        Session::SignedOut
    };
    Ok(Overview {
        session,
        secret_saved,
        devices,
    })
}

/// Forget one saved device; its workspace is left untouched in Coder.
pub(crate) fn forget_device(id: &str) -> Result<()> {
    Ok(cloud::remove(CloudProvider::Coder, id)?)
}

/// Delete a saved device's workspace at Coder, then forget the device. The
/// device stays saved until Coder confirms the workspace is gone, so a failed
/// or interrupted destroy can be retried from the same row.
pub(crate) fn destroy_device(
    settings: &Settings,
    id: &str,
    cancelled: impl Fn() -> bool,
    stage: impl FnMut(Stage),
) -> Result<()> {
    super::destroy::destroy(settings, &tokens(settings), id, cancelled, stage)?;
    forget_device(id)
}

/// Destroy the saved device `id` with the configured deployment, as the
/// sidebar's Remove does when asked to delete it at Coder too. A device no
/// longer saved has nothing left to do; one saved from another deployment is
/// refused. Blocks: run it off the UI thread.
pub(crate) fn destroy_saved(id: &str) -> crate::Result<()> {
    let Some(device) = cloud::load()?
        .into_iter()
        .find(|device| device.provider == CloudProvider::Coder && device.id == id)
    else {
        return Ok(());
    };
    let settings = crate::config::Config::load()?
        .coder
        .settings()?
        .ok_or(Error::Missing("url"))?;
    if settings.base != device.account {
        return Err(Error::Deployment.into());
    }
    Ok(destroy_device(&settings, id, || false, |_| {})?)
}

/// Save (or with `None`, forget) the OAuth client secret typed into Settings.
pub(crate) fn save_client_secret(
    store: crate::github::Store,
    secret: Option<&SecretString>,
) -> Result<()> {
    store::save_client_secret(store, secret)
}

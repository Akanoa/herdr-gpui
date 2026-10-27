//! Windows Credential Manager as the Windows keyring for saved sign-ins.
//! Each entry is a generic credential named `SERVICE/ACCOUNT`, kept for this
//! user on this computer only (`CRED_PERSIST_LOCAL_MACHINE`, never roamed to
//! other computers in a domain). A credential holds at most
//! `CRED_MAX_CREDENTIAL_BLOB_SIZE` bytes, so a larger secret, such as several
//! GitLab accounts in one record, continues in up to [`PARTS`] - 1 more
//! credentials named `SERVICE/ACCOUNT#partN`; one that fits is stored alone. Windows encrypts it with the user's logon
//! secrets, so other users cannot read it. The calls are synchronous and
//! cheap, but callers still run them on a background worker.
//!
//! `unsafe` is needed because the standard library and the workspace's
//! dependencies have no wrapper for the `Cred*` functions; the maintained
//! wrapper crates pull in a second `windows-sys` version. Each call is in its
//! own block that states what it relies on.

use crate::{Result, credential_bytes};
use secrecy::{ExposeSecret, SecretString};
use std::{io, iter::once};
use windows_sys::Win32::{
    Foundation::ERROR_NOT_FOUND,
    Security::Credentials::{
        CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW,
        CredDeleteW, CredFree, CredReadW, CredWriteW,
    },
};
use zeroize::{Zeroize, Zeroizing};

/// Why Credential Manager refused, with the Windows error as the source.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "Cannot read a saved sign-in from Windows Credential Manager. Sign in again, or set GH_TOKEN for GitHub."
    )]
    Read(#[source] io::Error),
    #[error("Windows Credential Manager update failed. Try again.")]
    Write(#[source] io::Error),
}

/// The most bytes one credential holds.
const PART: usize = CRED_MAX_CREDENTIAL_BLOB_SIZE as usize;
/// The most credentials one secret is split across: enough for the largest
/// record any store keeps ([`crate::LIMIT`]); a larger one is refused.
const PARTS: usize = crate::LIMIT.div_ceil(PART);

/// The credential's target name, which Credential Manager shows and keys on.
fn target(service: &str, account: &str) -> String {
    format!("{service}/{account}")
}

/// The target of part `index` of a split secret. The first part is the
/// entry's own target, so a secret that fits is stored as it always was.
fn part_target(service: &str, account: &str, index: usize) -> String {
    match index {
        0 => target(service, account),
        index => format!("{}#part{}", target(service, account), index + 1),
    }
}

/// A NUL-terminated UTF-16 copy for the wide-string API.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(once(0)).collect()
}

fn not_found(error: &io::Error) -> bool {
    error.raw_os_error() == i32::try_from(ERROR_NOT_FOUND).ok()
}

/// A credential `CredReadW` allocated. Its secret is wiped before it is freed.
struct Read(*mut CREDENTIALW);

impl Read {
    fn secret(&self) -> Vec<u8> {
        // SAFETY: `CredReadW` succeeded, so `self.0` points to a valid
        // CREDENTIALW whose `CredentialBlob` is null or holds exactly
        // `CredentialBlobSize` bytes, all alive until `CredFree` in `drop`.
        #[allow(unsafe_code)]
        unsafe {
            let credential = &*self.0;
            if credential.CredentialBlob.is_null() {
                return Vec::new();
            }
            std::slice::from_raw_parts(
                credential.CredentialBlob,
                credential.CredentialBlobSize as usize,
            )
            .to_vec()
        }
    }
}

impl Drop for Read {
    fn drop(&mut self) {
        // SAFETY: as in `secret`; the blob is writable memory this process
        // owns, and nothing uses the credential after `CredFree`.
        #[allow(unsafe_code)]
        unsafe {
            let credential = &*self.0;
            if !credential.CredentialBlob.is_null() {
                std::slice::from_raw_parts_mut(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                )
                .zeroize();
            }
            CredFree(self.0.cast_const().cast());
        }
    }
}

/// The entry `account` under `service`, or `None` when nothing is saved.
/// A split secret is joined from its parts, up to the first one missing.
pub(crate) fn read(service: &str, account: &str) -> Result<Option<SecretString>> {
    let Some(first) = read_part(&part_target(service, account, 0))? else {
        return Ok(None);
    };
    // Sized for every part up front, so no copy is left unwiped by a move.
    let mut secret = Zeroizing::new(Vec::with_capacity(PART * PARTS));
    secret.extend_from_slice(&Zeroizing::new(first));
    for index in 1..PARTS {
        let Some(part) = read_part(&part_target(service, account, index))? else {
            break;
        };
        secret.extend_from_slice(&Zeroizing::new(part));
    }
    credential_bytes(std::mem::take(&mut *secret)).map(Some)
}

/// One credential's bytes, or `None` when it does not exist.
fn read_part(target: &str) -> Result<Option<Vec<u8>>> {
    let name = wide(target);
    let mut credential = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated and outlives the call, and
    // `credential` is a valid out-pointer. On success it receives a buffer
    // the system allocated, which `Read` frees exactly once.
    #[allow(unsafe_code)]
    let found = unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if found == 0 {
        let error = io::Error::last_os_error();
        return if not_found(&error) {
            Ok(None)
        } else {
            Err(Error::Read(error).into())
        };
    }
    Ok(Some(Read(credential).secret()))
}

/// Saves `token` as `account` under `service`, with `label` as its comment,
/// or removes the entry when it is `None`. Removing an entry that does not
/// exist succeeds. A secret too large for one credential is split: the
/// parts no longer needed are removed first and the first part is written
/// last, so a reader never joins a new first part to old ones.
pub(crate) fn save(
    service: &str,
    label: &str,
    token: Option<&SecretString>,
    account: &str,
) -> Result<()> {
    let secret = token.map_or(&[][..], |token| token.expose_secret().as_bytes());
    let parts: Vec<&[u8]> = secret.chunks(PART).collect();
    if parts.len() > PARTS {
        return Err(Error::Write(io::ErrorKind::FileTooLarge.into()).into());
    }
    let kept = if token.is_some() {
        parts.len().max(1)
    } else {
        0
    };
    for index in (kept.max(1)..PARTS).rev() {
        delete(&wide(&part_target(service, account, index)))?;
    }
    if token.is_none() {
        return delete(&wide(&part_target(service, account, 0)));
    }
    for (index, part) in parts.iter().enumerate().skip(1).rev() {
        write_part(&part_target(service, account, index), label, account, part)?;
    }
    write_part(
        &part_target(service, account, 0),
        label,
        account,
        parts.first().copied().unwrap_or_default(),
    )
}

/// Writes one credential of at most [`PART`] bytes.
fn write_part(target: &str, label: &str, account: &str, secret: &[u8]) -> Result<()> {
    let size = u32::try_from(secret.len())
        .ok()
        .filter(|size| *size <= CRED_MAX_CREDENTIAL_BLOB_SIZE)
        .ok_or_else(|| crate::Error::from(Error::Write(io::ErrorKind::FileTooLarge.into())))?;
    let mut name = wide(target);
    let mut comment = wide(label);
    let mut user = wide(account);
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: name.as_mut_ptr(),
        Comment: comment.as_mut_ptr(),
        CredentialBlobSize: size,
        // `CredWriteW` only reads the blob; the field is `*mut` by declaration.
        CredentialBlob: secret.as_ptr().cast_mut(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: user.as_mut_ptr(),
        ..CREDENTIALW::default()
    };
    // SAFETY: every pointer in `credential` refers to a live buffer of the
    // stated length (strings NUL-terminated) that outlives the call, which
    // copies them and writes through none. It replaces an existing entry with
    // the same target in one step.
    #[allow(unsafe_code)]
    let written = unsafe { CredWriteW(&credential, 0) };
    if written == 0 {
        return Err(Error::Write(io::Error::last_os_error()).into());
    }
    Ok(())
}

fn delete(name: &[u16]) -> Result<()> {
    // SAFETY: `name` comes from `wide`, so it is NUL-terminated, and it
    // outlives the call.
    #[allow(unsafe_code)]
    let deleted = unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if deleted == 0 {
        let error = io::Error::last_os_error();
        if !not_found(&error) {
            return Err(Error::Write(error).into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

//! Accounts and their devices: creating an account, signing in and out, and
//! the device list.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use linuxplay_proto::rendezvous::{
    Credentials, DEVICE_NAME_MAX, Device, PASSWORD_MAX, PASSWORD_MIN, Session, valid_fingerprint, valid_username,
};

use crate::error::{ApiResult, Failure};
use crate::store::CreateError;
use crate::{App, Authed, ClientIp, secrets};

const WRONG_LOGIN: &str = "The username or password is wrong.";

/// Check the parts of `credentials` that don't need the database, and tidy
/// the device name.
fn checked(mut credentials: Credentials) -> ApiResult<Credentials> {
    if !valid_username(&credentials.username) {
        return Err(Failure::bad_request(
            "Usernames are 3 to 32 characters: letters, digits, dots, dashes and underscores.",
        ));
    }
    if !(PASSWORD_MIN..=PASSWORD_MAX).contains(&credentials.password.chars().count()) {
        return Err(Failure::bad_request(format!("Passwords need at least {PASSWORD_MIN} characters.")));
    }
    if !valid_fingerprint(&credentials.device.fingerprint) {
        return Err(Failure::bad_request("The device's fingerprint is malformed."));
    }
    let name: String = credentials.device.name.chars().filter(|c| !c.is_control()).take(DEVICE_NAME_MAX).collect();
    if name.trim().is_empty() {
        return Err(Failure::bad_request("The device needs a name."));
    }
    credentials.device.name = name.trim().to_string();
    credentials.device.fingerprint.make_ascii_lowercase();
    Ok(credentials)
}

/// Sign `credentials.device` in to `account_id` and build the reply.
async fn start_session(app: &App, account_id: i64, username: String, credentials: Credentials) -> ApiResult<Json<Session>> {
    let token = secrets::new_token();
    let stored = token.clone();
    let device = credentials.device;
    let device_id = app.db(move |store| store.sign_in(account_id, &device.name, &device.fingerprint, &stored)).await?;
    Ok(Json(Session { token, device_id, username }))
}

/// `POST /accounts`: create an account and sign this device in to it.
pub async fn create(
    State(app): State<Arc<App>>,
    ClientIp(ip): ClientIp,
    Json(credentials): Json<Credentials>,
) -> ApiResult<(StatusCode, Json<Session>)> {
    let credentials = checked(credentials)?;
    if !app.new_accounts.allow(&ip.to_string()) {
        return Err(Failure::rate_limited());
    }
    let password = credentials.password.clone();
    let hash = tokio::task::spawn_blocking(move || secrets::hash_password(&password))
        .await
        .map_err(Failure::internal)?
        .map_err(Failure::internal)?;
    let username = credentials.username.clone();
    let created = {
        let username = username.clone();
        let store = app.store.clone();
        tokio::task::spawn_blocking(move || store.create_account(&username, &hash)).await.map_err(Failure::internal)?
    };
    let account_id = match created {
        Ok(id) => id,
        Err(CreateError::UsernameTaken) => return Err(Failure::conflict("That username is taken.")),
        Err(CreateError::Database(e)) => return Err(Failure::internal(e)),
    };
    let session = start_session(&app, account_id, username, credentials).await?;
    Ok((StatusCode::CREATED, session))
}

/// `POST /sessions`: sign this device in.
pub async fn sign_in(
    State(app): State<Arc<App>>,
    ClientIp(ip): ClientIp,
    Json(credentials): Json<Credentials>,
) -> ApiResult<Json<Session>> {
    let credentials = checked(credentials).map_err(|_| Failure::unauthorized(WRONG_LOGIN))?;
    // Both the address and the account are limited: one stops a single
    // machine guessing, the other stops many machines guessing one account.
    let by_name = format!("user:{}", credentials.username.to_ascii_lowercase());
    if !app.sign_ins.allow(&ip.to_string()) || !app.sign_ins.allow(&by_name) {
        return Err(Failure::rate_limited());
    }
    let lookup = credentials.username.clone();
    let account = app.db(move |store| store.account(&lookup)).await?;
    // With no such account a stand-in hash is checked, so the reply takes as
    // long as a wrong password does and doesn't reveal which usernames exist.
    let (hash, found) = match &account {
        Some((_, _, hash)) => (hash.clone(), true),
        None => (app.stand_in_hash.clone(), false),
    };
    let password = credentials.password.clone();
    let matches = tokio::task::spawn_blocking(move || secrets::verify_password(&password, &hash))
        .await
        .map_err(Failure::internal)?;
    match account {
        Some((account_id, username, _)) if matches && found => start_session(&app, account_id, username, credentials).await,
        _ => Err(Failure::unauthorized(WRONG_LOGIN)),
    }
}

/// `DELETE /sessions/current`: sign this device out.
pub async fn sign_out(State(app): State<Arc<App>>, Authed(me): Authed) -> ApiResult<StatusCode> {
    app.relay.disconnect(&me.device_id);
    app.db(move |store| store.sign_out(&me.device_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /devices`: the account's devices.
pub async fn devices(State(app): State<Arc<App>>, Authed(me): Authed) -> ApiResult<Json<Vec<Device>>> {
    let account_id = me.account_id;
    let rows = app.db(move |store| store.devices(account_id)).await?;
    let devices = rows
        .into_iter()
        .map(|row| Device {
            online: app.relay.is_online(&row.id),
            current: row.id == me.device_id,
            id: row.id,
            name: row.name,
            fingerprint: row.fingerprint,
            last_seen: row.last_seen,
        })
        .collect();
    Ok(Json(devices))
}

/// `DELETE /devices/{id}`: remove a device from the account. It is signed
/// out, taken offline, and its invites stop working.
pub async fn remove_device(
    State(app): State<Arc<App>>,
    Authed(me): Authed,
    Path(device_id): Path<String>,
) -> ApiResult<StatusCode> {
    let target = device_id.clone();
    if !app.db(move |store| store.remove_device(me.account_id, &target)).await? {
        return Err(Failure::not_found("No such device in this account."));
    }
    app.relay.disconnect(&device_id);
    Ok(StatusCode::NO_CONTENT)
}

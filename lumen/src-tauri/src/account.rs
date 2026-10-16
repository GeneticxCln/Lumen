//! Tauri desktop adapter for account sign-in/sign-out/status commands.
//! Core PKCE, loopback callback parsing, and keychain token storage live in `lumen_core::account`.

pub use lumen_core::account::*;

use crate::error::{PlayerError, Result};
use serde_json::json;
use tauri_plugin_opener::OpenerExt;
use tokio::net::TcpListener;
use url::Url;

/// Opens the system browser, waits for the loopback callback, and stores the app token.
#[tauri::command]
pub async fn account_sign_in(app: tauri::AppHandle) -> Result<AccountStatus> {
    let origin = account_origin();
    let verifier = random_secret();
    let challenge = s256_challenge(&verifier);
    let state = random_secret();

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let mut authorize = Url::parse(&format!("{origin}/desktop/authorize"))
        .map_err(|e| PlayerError::InvalidArg(format!("Bad account address: {e}")))?;
    authorize
        .query_pairs_mut()
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");

    app.opener()
        .open_url(authorize.as_str(), None::<&str>)
        .map_err(|e| PlayerError::Io(format!("Could not open the browser: {e}")))?;

    let code = tokio::time::timeout(SIGN_IN_TIMEOUT, wait_for_code(&listener, &state))
        .await
        .map_err(|_| PlayerError::Io("Sign-in timed out. Try again.".into()))??;

    let client = http_client()?;
    let response = client
        .post(format!("{origin}/api/desktop/token"))
        .json(&json!({ "code": code, "code_verifier": verifier, "redirect_uri": redirect_uri }))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(PlayerError::Http(format!("Sign-in was refused ({})", response.status())));
    }
    let token: TokenResponse = response.json().await?;
    store_app_token(&token.access_token)?;

    let check = check_token(&origin, &token.access_token).await;
    if matches!(check, TokenCheck::Rejected) {
        clear_app_token()?;
        return Err(PlayerError::Http("Sign-in did not produce a usable session".into()));
    }
    // Push this device's profiles, My List and progress, and pull the account's copies.
    if let Ok(pool) = crate::commands::ensure_db(&app).await {
        crate::sync::schedule(pool);
    }
    match check {
        TokenCheck::Valid(sub) => Ok(AccountStatus { signed_in: true, sub: Some(sub), verified: true }),
        _ => Ok(AccountStatus { signed_in: true, sub: None, verified: false }),
    }
}

/// Reports the signed-in account. A token the server rejects is removed locally.
#[tauri::command]
pub async fn account_status() -> Result<AccountStatus> {
    let Some(token) = load_app_token()? else {
        return Ok(AccountStatus::signed_out());
    };
    match check_token(&account_origin(), &token).await {
        TokenCheck::Valid(sub) => Ok(AccountStatus { signed_in: true, sub: Some(sub), verified: true }),
        TokenCheck::Rejected => {
            clear_app_token()?;
            Ok(AccountStatus::signed_out())
        }
        TokenCheck::Unreachable => Ok(AccountStatus { signed_in: true, sub: None, verified: false }),
    }
}

/// Revokes the app token on the server when reachable, then removes it locally either way.
#[tauri::command]
pub async fn account_sign_out(app: tauri::AppHandle) -> Result<AccountStatus> {
    if let Some(token) = load_app_token()? {
        if let Ok(client) = http_client() {
            let _ = client
                .post(format!("{}/api/desktop/signout", account_origin()))
                .bearer_auth(&token)
                .send()
                .await;
        }
        clear_app_token()?;
    }
    let pool = crate::commands::ensure_db(&app).await?;
    crate::sync::forget(pool).await?;
    Ok(AccountStatus::signed_out())
}

use crate::*;
use axum::{Extension, extract::Path};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
    pub name: Option<String>,
}

async fn auth_budget(app: &App, headers: &HeaderMap, email: &str) -> Result<()> {
    origin_check(app, headers, false)?;
    // Vercel sets x-real-ip and overwrites x-forwarded-for, so neither is client-controlled there.
    let ip = headers
        .get("x-real-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|h| h.to_str().ok())
        .unwrap_or("local")
        .split(',')
        .next()
        .unwrap_or("local")
        .trim();
    for key in [
        format!("auth:ip:{}", ozy_auth::token_hash(ip)),
        format!("auth:email:{}", ozy_auth::token_hash(email)),
    ] {
        if !ozy_db::take_budget(&app.db, &key, 900, 15, 1).await? {
            return Err(Failure(
                StatusCode::TOO_MANY_REQUESTS,
                "auth_rate_limited",
                "Try again after the 15-minute authentication window".into(),
            ));
        }
    }
    Ok(())
}
fn email(raw: &str) -> Result<String> {
    let email = raw.trim().to_ascii_lowercase();
    if email.len() > 254 || !email.is_ascii() || !email_address::EmailAddress::is_valid(&email) {
        return Err(invalid("A valid email address is required"));
    }
    Ok(email)
}
fn cookie_value(app: &App, token: &str, clear: bool) -> String {
    let secure = app.origin.starts_with("https://");
    format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        if secure { "__Host-ozy_session" } else { "ozy_session" },
        token,
        if clear { 0 } else { 604800 },
        if secure { "; Secure" } else { "" }
    )
}
async fn session(app: &App, user: Uuid) -> Result<Response> {
    let token = ozy_auth::token("ses_");
    sqlx::query("INSERT INTO ozy.sessions(token_hash,user_id,expires_at) VALUES($1,$2,now()+interval '7 days')")
        .bind(ozy_auth::token_hash(&token))
        .bind(user)
        .execute(&app.db)
        .await?;
    let mut response = Json(json!({"user_id":user,"email_verified":false})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie_value(app, &token, false).parse().map_err(|_| invalid("Cookie error"))?,
    );
    Ok(response)
}
pub async fn register(State(app): State<App>, headers: HeaderMap, Json(input): Json<Credentials>) -> Result<Response> {
    let email = email(&input.email)?;
    auth_budget(&app, &headers, &email).await?;
    if !ozy_db::take_budget(&app.db, "registrations:global", 86400, 100, 1).await? {
        return Err(Failure(
            StatusCode::TOO_MANY_REQUESTS,
            "registration_limit",
            "Daily registration capacity reached".into(),
        ));
    }
    let name = input.name.unwrap_or_else(|| "Developer".into()).trim().to_owned();
    if name.is_empty() || name.len() > 80 {
        return Err(invalid("Name must be 1–80 bytes"));
    }
    let _permit = app.crypto_slots.acquire().await.map_err(|_| unauthorized())?;
    let hash = tokio::task::spawn_blocking(move || ozy_auth::hash_password(&input.password))
        .await
        .map_err(|_| invalid("Password processing failed"))?
        .map_err(|e| invalid(&e.to_string()))?;
    let id = Uuid::new_v4();
    let inserted =
        sqlx::query("INSERT INTO ozy.users(id,email,password_hash,display_name) VALUES($1,$2,$3,$4) ON CONFLICT(email) DO NOTHING")
            .bind(id)
            .bind(email)
            .bind(hash)
            .bind(name)
            .execute(&app.db)
            .await?
            .rows_affected();
    if inserted == 0 {
        return Err(Failure(
            StatusCode::CONFLICT,
            "registration_failed",
            "Unable to register with these details; try signing in".into(),
        ));
    }
    session(&app, id).await
}
pub async fn login(State(app): State<App>, headers: HeaderMap, Json(input): Json<Credentials>) -> Result<Response> {
    let email = email(&input.email)?;
    auth_budget(&app, &headers, &email).await?;
    let user = sqlx::query("SELECT id,password_hash FROM ozy.users WHERE email=$1")
        .bind(email)
        .fetch_optional(&app.db)
        .await?;
    let _permit = app.crypto_slots.acquire().await.map_err(|_| unauthorized())?;
    let hash = user.as_ref().map(|r| r.get::<String, _>("password_hash"));
    let valid = tokio::task::spawn_blocking(move || match hash {
        Some(hash) => ozy_auth::verify_password(&input.password, &hash),
        None => {
            let _ = ozy_auth::hash_password("constant dummy password");
            false
        }
    })
    .await
    .map_err(|_| unauthorized())?;
    if !valid {
        return Err(Failure(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "Email or password is incorrect".into(),
        ));
    }
    session(&app, user.ok_or_else(unauthorized)?.get("id")).await
}
/// Public, always 200: lets pages check sign-in state without logging a 401.
pub async fn session_state(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    let Some(token) = session_token(&headers).filter(|t| t.len() <= 100) else {
        return Ok(Json(json!({"signed_in":false})));
    };
    let valid: Option<Uuid> = sqlx::query_scalar("SELECT user_id FROM ozy.sessions WHERE token_hash=$1 AND expires_at>now()")
        .bind(ozy_auth::token_hash(&token))
        .fetch_optional(&app.db)
        .await?;
    Ok(Json(json!({"signed_in":valid.is_some()})))
}
pub async fn logout(State(app): State<App>, headers: HeaderMap) -> Result<Response> {
    if let Some(token) = session_token(&headers) {
        sqlx::query("DELETE FROM ozy.sessions WHERE token_hash=$1")
            .bind(ozy_auth::token_hash(&token))
            .execute(&app.db)
            .await?;
    }
    let mut response = Json(json!({"signed_out":true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie_value(&app, "", true).parse().map_err(|_| invalid("Cookie error"))?,
    );
    Ok(response)
}
pub async fn me(State(app): State<App>, Extension(id): Extension<Identity>) -> Result<Json<Value>> {
    let user:Value=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'email',email,'name',display_name,'email_verified',email_verified,'created_at',created_at) FROM ozy.users WHERE id=$1")
        .bind(id.user_id).fetch_one(&app.db).await?;
    Ok(Json(
        json!({"user":user,"limits":{"units_per_day":app.limits.per_day,"units_per_minute":app.limits.per_minute,"keys":10},"email_delivery_configured":false}),
    ))
}
#[derive(Deserialize)]
pub struct AccountUpdate {
    name: String,
}
pub async fn update_account(
    State(app): State<App>,
    Extension(id): Extension<Identity>,
    Json(input): Json<AccountUpdate>,
) -> Result<Json<Value>> {
    let name = input.name.trim();
    if name.is_empty() || name.len() > 80 {
        return Err(invalid("Name must be 1–80 bytes"));
    }
    sqlx::query("UPDATE ozy.users SET display_name=$1 WHERE id=$2")
        .bind(name)
        .bind(id.user_id)
        .execute(&app.db)
        .await?;
    Ok(Json(json!({"updated":true})))
}
#[derive(Deserialize)]
pub struct AccountDeletion {
    password: String,
}
/// Permanently deletes the account and, by cascade, its keys, logs, monitors,
/// extractions, crawls, webhooks and sessions. Requires the current password.
pub async fn delete_account(
    State(app): State<App>,
    Extension(id): Extension<Identity>,
    Json(input): Json<AccountDeletion>,
) -> Result<Response> {
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM ozy.users WHERE id=$1")
        .bind(id.user_id)
        .fetch_one(&app.db)
        .await?;
    let _permit = app.crypto_slots.acquire().await.map_err(|_| unauthorized())?;
    let valid = tokio::task::spawn_blocking(move || ozy_auth::verify_password(&input.password, &hash))
        .await
        .map_err(|_| unauthorized())?;
    if !valid {
        return Err(Failure(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "Password is incorrect".into(),
        ));
    }
    sqlx::query("DELETE FROM ozy.users WHERE id=$1")
        .bind(id.user_id)
        .execute(&app.db)
        .await?;
    let mut response = Json(json!({"deleted":true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie_value(&app, "", true).parse().map_err(|_| invalid("Cookie error"))?,
    );
    Ok(response)
}
#[derive(Deserialize)]
pub struct PasswordUpdate {
    current_password: String,
    new_password: String,
}
pub async fn change_password(
    State(app): State<App>,
    Extension(id): Extension<Identity>,
    Json(input): Json<PasswordUpdate>,
) -> Result<Json<Value>> {
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM ozy.users WHERE id=$1")
        .bind(id.user_id)
        .fetch_one(&app.db)
        .await?;
    let _permit = app.crypto_slots.acquire().await.map_err(|_| unauthorized())?;
    let hash = tokio::task::spawn_blocking(move || {
        if !ozy_auth::verify_password(&input.current_password, &hash) {
            return Err(ozy_auth::Error::Crypto);
        }
        ozy_auth::hash_password(&input.new_password)
    })
    .await
    .map_err(|_| unauthorized())?
    .map_err(|_| invalid("Current password is incorrect or new password is invalid"))?;
    let mut tx = app.db.begin().await?;
    sqlx::query("UPDATE ozy.users SET password_hash=$1 WHERE id=$2")
        .bind(hash)
        .bind(id.user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM ozy.sessions WHERE user_id=$1")
        .bind(id.user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"updated":true,"sign_in_required":true})))
}
pub async fn keys(State(app): State<App>, Extension(id): Extension<Identity>) -> Result<Json<Value>> {
    let items:Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(x ORDER BY x.created_at DESC),'[]') FROM (SELECT id,name,prefix,permissions,created_at,last_used_at,revoked_at FROM ozy.api_keys WHERE user_id=$1) x")
        .bind(id.user_id).fetch_one(&app.db).await?;
    Ok(Json(json!({"keys":items})))
}
#[derive(Deserialize)]
pub struct KeyInput {
    name: String,
    permissions: Option<Vec<String>>,
}
pub async fn create_key(State(app): State<App>, Extension(id): Extension<Identity>, Json(input): Json<KeyInput>) -> Result<Json<Value>> {
    if input.name.trim().is_empty() || input.name.len() > 64 {
        return Err(invalid("Key name must be 1–64 bytes"));
    }
    let permissions = input.permissions.unwrap_or_else(|| vec!["*".into()]);
    let allowed = [
        "*",
        "scrape",
        "preview",
        "intelligence",
        "performance",
        "url",
        "email",
        "ocr",
        "screenshot",
        "pdf",
        "monitor",
        "extract",
        "crawls",
        "webhooks",
    ];
    if permissions.is_empty() || permissions.len() > 14 || permissions.iter().any(|p| !allowed.contains(&p.as_str())) {
        return Err(invalid("Invalid key permissions"));
    }
    let mut tx = app.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(id.user_id.to_string())
        .execute(&mut *tx)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ozy.api_keys WHERE user_id=$1 AND revoked_at IS NULL")
        .bind(id.user_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= 10 {
        return Err(invalid("Maximum 10 active API keys"));
    }
    let token = ozy_auth::token("ozy_");
    let key_id = Uuid::new_v4();
    sqlx::query("INSERT INTO ozy.api_keys(id,user_id,name,prefix,token_hash,permissions) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(key_id)
        .bind(id.user_id)
        .bind(input.name)
        .bind(&token[..12])
        .bind(ozy_auth::token_hash(&token))
        .bind(permissions)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"id":key_id,"key":token,"notice":"Copy this key now. It cannot be retrieved again."}),
    ))
}
pub async fn revoke_key(State(app): State<App>, Extension(id): Extension<Identity>, Path(key): Path<Uuid>) -> Result<Json<Value>> {
    let result = sqlx::query("UPDATE ozy.api_keys SET revoked_at=now() WHERE id=$1 AND user_id=$2")
        .bind(key)
        .bind(id.user_id)
        .execute(&app.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(missing());
    }
    Ok(Json(json!({"revoked":true})))
}
pub async fn usage(State(app): State<App>, Extension(id): Extension<Identity>) -> Result<Json<Value>> {
    let daily:Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(x ORDER BY x.day),'[]') FROM (SELECT date_trunc('day',created_at) AS day,count(*) AS requests,sum(units) AS units,round(avg(duration_ms)) AS average_ms,count(*) FILTER(WHERE status>=400) AS errors FROM ozy.request_logs WHERE user_id=$1 AND created_at>now()-interval '30 days' GROUP BY 1) x")
        .bind(id.user_id).fetch_one(&app.db).await?;
    let per_key:Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(x),'[]') FROM (SELECT key_id,count(*) AS requests,sum(units) AS units FROM ozy.request_logs WHERE user_id=$1 AND created_at>now()-interval '30 days' GROUP BY key_id) x")
        .bind(id.user_id).fetch_one(&app.db).await?;
    Ok(Json(json!({"daily":daily,"per_key":per_key,"retention_days":30})))
}
pub async fn logs(State(app): State<App>, Extension(id): Extension<Identity>) -> Result<Json<Value>> {
    let logs:Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(x ORDER BY x.created_at DESC),'[]') FROM (SELECT id,key_id,endpoint,status,duration_ms,units,created_at FROM ozy.request_logs WHERE user_id=$1 ORDER BY created_at DESC LIMIT 100) x")
        .bind(id.user_id).fetch_one(&app.db).await?;
    Ok(Json(json!({"logs":logs})))
}
pub async fn notifications(State(app): State<App>, Extension(id): Extension<Identity>) -> Result<Json<Value>> {
    let items:Value=sqlx::query_scalar("SELECT COALESCE(jsonb_agg(x ORDER BY x.created_at DESC),'[]') FROM (SELECT id,kind,message,created_at FROM ozy.notifications WHERE user_id=$1 ORDER BY created_at DESC LIMIT 50) x")
        .bind(id.user_id).fetch_one(&app.db).await?;
    Ok(Json(json!({"notifications":items})))
}

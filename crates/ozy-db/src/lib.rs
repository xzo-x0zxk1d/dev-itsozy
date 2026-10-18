use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{str::FromStr, time::Duration};

/// Connection options for the platform database.
///
/// sqlx always uses named prepared statements for bound queries, which collide on
/// transaction-mode poolers (Supabase Supavisor port 6543, PgBouncer). Supabase's
/// pooler also serves session mode on port 5432 of the same host, so 6543 is
/// rewritten to 5432 there. Session mode pins one backend per pooled client, so the
/// pool below is deliberately small and releases idle connections quickly.
pub fn options(url: &str) -> Result<PgConnectOptions, sqlx::Error> {
    let options = PgConnectOptions::from_str(url)?;
    if options.get_port() == 6543 && options.get_host().ends_with(".pooler.supabase.com") {
        return Ok(options.port(5432));
    }
    Ok(options)
}

/// Lazily connected so a cold start never fails on a transient database outage.
pub fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    Ok(PgPoolOptions::new()
        .max_connections(3)
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(8))
        .idle_timeout(Duration::from_secs(15))
        .max_lifetime(Duration::from_secs(300))
        .connect_lazy_with(options(url)?))
}

/// Migrations take a session-level advisory lock; use a direct or session-mode URL.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("../../migrations").run(pool).await
}

/// Atomic shared rate limit; a failed increment leaves the current counter intact.
pub async fn take_budget(pool: &PgPool, key: &str, window_seconds: i64, limit: i32, cost: i32) -> Result<bool, sqlx::Error> {
    if cost > limit {
        return Ok(false);
    }
    let granted = sqlx::query_scalar::<_, i32>(
        "INSERT INTO ozy.budgets (key,bucket,used) VALUES ($1,floor(extract(epoch FROM now())/$2)::bigint,$3)
         ON CONFLICT(key,bucket) DO UPDATE SET used=ozy.budgets.used+EXCLUDED.used
         WHERE ozy.budgets.used+EXCLUDED.used <= $4 RETURNING used",
    )
    .bind(key)
    .bind(window_seconds as f64)
    .bind(cost)
    .bind(limit)
    .fetch_optional(pool)
    .await?;
    Ok(granted.is_some())
}

#[cfg(test)]
mod tests {
    #[test]
    fn supabase_transaction_pooler_is_rewritten_to_session_mode() {
        let options = super::options("postgres://u:p@aws-0-us-east-1.pooler.supabase.com:6543/postgres?sslmode=require").unwrap();
        assert_eq!(options.get_port(), 5432);
        let other = super::options("postgres://u:p@db.internal.example:6543/postgres").unwrap();
        assert_eq!(other.get_port(), 6543);
    }
}

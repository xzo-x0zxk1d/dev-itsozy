//! Operations tool.
//!
//!   ozy-ops [--env-file PATH] migrate      apply migrations (session-mode URL preferred)
//!   ozy-ops [--env-file PATH] status       list applied migrations and row counts
//!   ozy-ops [--env-file PATH] schedule URL install the Supabase pg_cron job that calls URL/api/v1/internal/tick every minute
//!   ozy-ops [--env-file PATH] unschedule   remove that job
//!   ozy-ops [--env-file PATH] purge-test-users   delete accounts created by tests/smoke checks (@example.com prefixes)
//!
//! Secrets are read from the environment and never printed.
use sqlx::Row;

const JOB: &str = "ozy-tick";
const VAULT_SECRET: &str = "ozy_cron_secret";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--env-file") {
        let path = args.get(1).ok_or("--env-file needs a path")?.clone();
        dotenvy::from_filename(&path)?;
        args.drain(..2);
    } else {
        let _ = dotenvy::dotenv();
    }
    // Migrations and pg_cron administration need a session (non-transaction-pooled) connection.
    let url = std::env::var("POSTGRES_URL_NON_POOLING")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .or_else(|_| std::env::var("POSTGRES_URL"))
        .map_err(|_| "Set POSTGRES_URL_NON_POOLING, DATABASE_URL or POSTGRES_URL")?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(ozy_db::options(&url)?)
        .await?;
    match args.first().map(String::as_str).unwrap_or("migrate") {
        "migrate" => {
            ozy_db::migrate(&pool).await?;
            println!("Migrations applied");
        }
        "status" => {
            for row in sqlx::query("SELECT version,description,success FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await?
            {
                println!(
                    "migration {} {} success={}",
                    row.get::<i64, _>(0),
                    row.get::<String, _>(1),
                    row.get::<bool, _>(2)
                );
            }
            for table in [
                "users",
                "api_keys",
                "request_logs",
                "monitors",
                "extractions",
                "crawls",
                "webhooks",
                "webhook_events",
            ] {
                let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM ozy.{table}"))
                    .fetch_one(&pool)
                    .await?;
                println!("ozy.{table}: {count} rows");
            }
            let jobs: Vec<(String, String)> = sqlx::query_as("SELECT jobname, schedule FROM cron.job WHERE jobname=$1")
                .bind(JOB)
                .fetch_all(&pool)
                .await
                .unwrap_or_default();
            println!(
                "pg_cron job: {}",
                jobs.first()
                    .map(|(n, s)| format!("{n} ({s})"))
                    .unwrap_or_else(|| "not installed".into())
            );
        }
        "schedule" => {
            let origin = args.get(1).ok_or("schedule needs the public origin, e.g. https://dev.itsozy.lol")?;
            let url = url::Url::parse(origin)?.join("/api/v1/internal/tick")?;
            if url.scheme() != "https" {
                return Err("The scheduler target must be https".into());
            }
            let secret = std::env::var("CRON_SECRET").map_err(|_| "CRON_SECRET must be set in the environment")?;
            sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_cron").execute(&pool).await?;
            sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_net").execute(&pool).await?;
            // Store the bearer secret in Supabase Vault so it never appears in cron.job.
            let existing: Option<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM vault.secrets WHERE name=$1")
                .bind(VAULT_SECRET)
                .fetch_optional(&pool)
                .await?;
            match existing {
                Some(id) => {
                    sqlx::query("SELECT vault.update_secret($1,$2)")
                        .bind(id)
                        .bind(&secret)
                        .execute(&pool)
                        .await?
                }
                None => {
                    sqlx::query("SELECT vault.create_secret($1,$2)")
                        .bind(&secret)
                        .bind(VAULT_SECRET)
                        .execute(&pool)
                        .await?
                }
            };
            let command = format!(
                "SELECT net.http_post(url := '{url}', headers := jsonb_build_object('Authorization', 'Bearer ' || (SELECT decrypted_secret FROM vault.decrypted_secrets WHERE name = '{VAULT_SECRET}'), 'Content-Type', 'application/json'), body := '{{}}'::jsonb, timeout_milliseconds := 55000)"
            );
            sqlx::query("SELECT cron.schedule($1,'* * * * *',$2)")
                .bind(JOB)
                .bind(command)
                .execute(&pool)
                .await?;
            println!("Scheduled {JOB}: every minute -> {url} (secret stored in Supabase Vault)");
        }
        "purge-test-users" => {
            // Accounts created by the automated tests and smoke checks; cascades all their data.
            let deleted = sqlx::query(
                "DELETE FROM ozy.users WHERE email LIKE '%@example.com' AND (email LIKE 'it-%' OR email LIKE 'quota-%'
                 OR email LIKE 'worker-e2e-%' OR email LIKE 'prod-smoke-%' OR email LIKE 'prod-debug-%')",
            )
            .execute(&pool)
            .await?
            .rows_affected();
            println!("Deleted {deleted} test accounts");
        }
        "unschedule" => {
            sqlx::query("SELECT cron.unschedule($1)").bind(JOB).execute(&pool).await?;
            println!("Removed {JOB}");
        }
        other => return Err(format!("Unknown command {other}").into()),
    }
    pool.close().await;
    Ok(())
}

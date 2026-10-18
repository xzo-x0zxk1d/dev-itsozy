CREATE SCHEMA IF NOT EXISTS ozy;
REVOKE ALL ON SCHEMA ozy FROM PUBLIC;
CREATE TABLE ozy.users (
 id uuid PRIMARY KEY, email text NOT NULL UNIQUE, password_hash text NOT NULL,
 display_name text NOT NULL, email_verified boolean NOT NULL DEFAULT false,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE ozy.sessions (
 token_hash text PRIMARY KEY, user_id uuid NOT NULL REFERENCES ozy.users(id) ON DELETE CASCADE,
 expires_at timestamptz NOT NULL, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX sessions_expiry ON ozy.sessions(expires_at);
CREATE TABLE ozy.api_keys (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES ozy.users(id) ON DELETE CASCADE,
 name text NOT NULL, prefix text NOT NULL, token_hash text NOT NULL UNIQUE,
 permissions text[] NOT NULL, revoked_at timestamptz, created_at timestamptz NOT NULL DEFAULT now(),
 last_used_at timestamptz
);
CREATE INDEX api_keys_owner ON ozy.api_keys(user_id);
CREATE TABLE ozy.budgets (
 key text NOT NULL, bucket bigint NOT NULL, used integer NOT NULL CHECK(used>=0),
 created_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY(key,bucket)
);
CREATE TABLE ozy.request_logs (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES ozy.users(id) ON DELETE CASCADE,
 key_id uuid REFERENCES ozy.api_keys(id) ON DELETE SET NULL, endpoint text NOT NULL,
 status integer NOT NULL, duration_ms bigint NOT NULL, units integer NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX request_logs_owner_time ON ozy.request_logs(user_id,created_at DESC);
CREATE TABLE ozy.fingerprints (
 user_id uuid NOT NULL REFERENCES ozy.users(id) ON DELETE CASCADE, origin text NOT NULL,
 identifier text NOT NULL, fingerprint jsonb NOT NULL, updated_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(user_id,origin,identifier)
);
CREATE TABLE ozy.cache (
 user_id uuid NOT NULL REFERENCES ozy.users(id) ON DELETE CASCADE,
 key text NOT NULL, value jsonb NOT NULL, expires_at timestamptz NOT NULL,
 PRIMARY KEY(user_id,key)
);
CREATE INDEX cache_expiry ON ozy.cache(expires_at);
CREATE TABLE ozy.monitors (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES ozy.users(id) ON DELETE CASCADE,
 url text NOT NULL, interval_seconds integer NOT NULL CHECK(interval_seconds BETWEEN 300 AND 86400),
 webhook_url text, enabled boolean NOT NULL DEFAULT true, next_check timestamptz NOT NULL DEFAULT now(),
 content_hash text, last_status integer, lease_until timestamptz, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX monitors_due ON ozy.monitors(next_check) WHERE enabled;
CREATE TABLE ozy.monitor_checks (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, monitor_id uuid NOT NULL REFERENCES ozy.monitors(id) ON DELETE CASCADE,
 status integer, duration_ms bigint NOT NULL, available boolean NOT NULL,
 changed boolean NOT NULL, checked_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX checks_monitor_time ON ozy.monitor_checks(monitor_id,checked_at DESC);
CREATE TABLE ozy.extractions (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES ozy.users(id) ON DELETE CASCADE,
 name text NOT NULL, url text NOT NULL, rules jsonb NOT NULL,
 refresh_seconds integer NOT NULL CHECK(refresh_seconds BETWEEN 300 AND 86400),
 next_refresh timestamptz NOT NULL DEFAULT now(), last_result jsonb,
 last_error text, lease_until timestamptz, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX extractions_due ON ozy.extractions(next_refresh);
CREATE TABLE ozy.crawls (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES ozy.users(id) ON DELETE CASCADE,
 config jsonb NOT NULL, checkpoint jsonb NOT NULL, state text NOT NULL DEFAULT 'queued',
 lease_until timestamptz, created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE ozy.webhooks (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES ozy.users(id) ON DELETE CASCADE,
 destination text NOT NULL, secret_ciphertext text NOT NULL, event_types text[] NOT NULL,
 enabled boolean NOT NULL DEFAULT true, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE ozy.webhook_events (
 id uuid PRIMARY KEY, webhook_id uuid NOT NULL REFERENCES ozy.webhooks(id) ON DELETE CASCADE,
 external_id text NOT NULL, event_type text NOT NULL, payload jsonb NOT NULL,
 state text NOT NULL DEFAULT 'pending', attempts integer NOT NULL DEFAULT 0,
 next_attempt timestamptz NOT NULL DEFAULT now(), lease_until timestamptz,
 last_status integer, last_error text, created_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(webhook_id,external_id)
);
CREATE INDEX events_due ON ozy.webhook_events(next_attempt) WHERE state='pending';
CREATE TABLE ozy.notifications (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES ozy.users(id) ON DELETE CASCADE,
 kind text NOT NULL, message text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), read_at timestamptz
);
CREATE TABLE ozy.domain_leases (
 host text PRIMARY KEY, next_request timestamptz NOT NULL
);
-- Private schema is not exposed by Supabase's REST API. Also deny public roles.
REVOKE ALL ON ALL TABLES IN SCHEMA ozy FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA ozy FROM PUBLIC;
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='anon') THEN
  REVOKE ALL ON SCHEMA ozy FROM anon;
  REVOKE ALL ON ALL TABLES IN SCHEMA ozy FROM anon;
 END IF;
 IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='authenticated') THEN
  REVOKE ALL ON SCHEMA ozy FROM authenticated;
  REVOKE ALL ON ALL TABLES IN SCHEMA ozy FROM authenticated;
 END IF;
END $$;

-- Monitoring, saved extractions, webhook relay delivery logs and scheduler state.
ALTER TABLE ozy.monitors
  ADD COLUMN selector text,
  ADD COLUMN webhook_secret_ciphertext text,
  ADD COLUMN last_available boolean,
  ADD COLUMN last_checked_at timestamptz,
  ADD COLUMN last_error text;
ALTER TABLE ozy.monitor_checks ADD COLUMN error text;
ALTER TABLE ozy.extractions
  ADD COLUMN markdown boolean NOT NULL DEFAULT false,
  ADD COLUMN fetch_count bigint NOT NULL DEFAULT 0,
  ADD COLUMN refresh_count bigint NOT NULL DEFAULT 0,
  ADD COLUMN last_refreshed_at timestamptz,
  ADD COLUMN last_accessed_at timestamptz;
ALTER TABLE ozy.crawls ADD COLUMN error text;
CREATE INDEX monitors_owner ON ozy.monitors(user_id);
CREATE INDEX extractions_owner ON ozy.extractions(user_id);
CREATE INDEX webhooks_owner ON ozy.webhooks(user_id);
CREATE INDEX crawls_owner ON ozy.crawls(user_id);
CREATE INDEX crawls_queued ON ozy.crawls(updated_at) WHERE state='queued';
CREATE INDEX request_logs_time ON ozy.request_logs(created_at);
CREATE INDEX budgets_created ON ozy.budgets(created_at);
CREATE INDEX notifications_owner ON ozy.notifications(user_id, created_at DESC);
CREATE TABLE ozy.webhook_attempts (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 event_id uuid NOT NULL REFERENCES ozy.webhook_events(id) ON DELETE CASCADE,
 attempt integer NOT NULL, status integer, error text, duration_ms bigint NOT NULL,
 attempted_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX webhook_attempts_event ON ozy.webhook_attempts(event_id, attempted_at DESC);
CREATE TABLE ozy.system_state (
 key text PRIMARY KEY, value jsonb NOT NULL, updated_at timestamptz NOT NULL DEFAULT now()
);
-- Defense in depth: no policies means non-owner roles see nothing even if granted.
DO $$ DECLARE t record; BEGIN
 FOR t IN SELECT tablename FROM pg_tables WHERE schemaname='ozy' LOOP
  EXECUTE format('ALTER TABLE ozy.%I ENABLE ROW LEVEL SECURITY', t.tablename);
 END LOOP;
END $$;
REVOKE ALL ON ALL TABLES IN SCHEMA ozy FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA ozy FROM PUBLIC;
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='anon') THEN
  REVOKE ALL ON ALL TABLES IN SCHEMA ozy FROM anon;
 END IF;
 IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname='authenticated') THEN
  REVOKE ALL ON ALL TABLES IN SCHEMA ozy FROM authenticated;
 END IF;
END $$;

ALTER TABLE bug_report_submission DROP CONSTRAINT bug_report_submission_sink_profile_id_check;
ALTER TABLE bug_report_submission ADD CHECK (sink_profile_id IN ('disabled','github'));
-- 過去の disabled 提出は principal が NULL のまま、再同意なしに配送しない。
ALTER TABLE bug_report_submission ADD COLUMN principal jsonb;
ALTER TABLE bug_report_delivery
 ADD COLUMN attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
 ADD COLUMN next_attempt_at timestamptz NOT NULL DEFAULT now(),
 ADD COLUMN created_at timestamptz NOT NULL DEFAULT now(),
 ADD COLUMN lease_owner uuid,
 ADD COLUMN lease_until timestamptz,
 ADD COLUMN fence bigint NOT NULL DEFAULT 0,
 ADD COLUMN external_id text,
 ADD COLUMN url text,
 ADD COLUMN error_code text,
 ADD COLUMN outcome text NOT NULL DEFAULT 'queued' CHECK (outcome IN ('queued','sending','sent','failed','delivery_unknown','held'));
CREATE INDEX bug_report_delivery_due ON bug_report_delivery (next_attempt_at,tenant_id,org)
 WHERE outcome IN ('queued','sending','delivery_unknown');

-- installation は全 tenant 共通の外部 quota。本文・tenant/org を持たない制御情報のみ
-- を共有するため RLS の対象外（報告本体・submission・delivery の RLS は維持）。
CREATE TABLE bug_report_destination_cooldown (
 installation_id text PRIMARY KEY,
 next_attempt_at timestamptz NOT NULL
);
GRANT SELECT, INSERT, UPDATE ON bug_report_destination_cooldown TO shiki_runtime;

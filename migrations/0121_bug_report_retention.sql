-- 本文消去後も配送 ID・hash・同意日時・監査用メタデータを保持する。
ALTER TABLE bug_report ADD COLUMN body_purged_at timestamptz;
ALTER TABLE bug_report_submission ADD COLUMN body_purged_at timestamptz;
CREATE INDEX bug_report_retention ON bug_report (tenant_id, org, updated_at)
 WHERE body_purged_at IS NULL;
CREATE INDEX help_case_expiry ON help_resolution_case (expires_at, tenant_id, org);

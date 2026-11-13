-- PR1: ローカル受付。配送 lease/fence・GitHub 結果列は PR2 で追加する。
CREATE TABLE bug_report_policy (
    tenant_id text NOT NULL, org text NOT NULL,
    settings jsonb NOT NULL, rev bigint NOT NULL CHECK (rev > 0),
    updated_by text NOT NULL, updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org)
);
CREATE TABLE bug_report (
    tenant_id text NOT NULL, org text NOT NULL, id uuid NOT NULL,
    reporter_id text NOT NULL, status text NOT NULL CHECK (status IN
      ('draft','pending_approval','queued','held','rejected','cancelled','sending','sent','failed','delivery_unknown')),
    rev bigint NOT NULL CHECK (rev > 0), snapshot jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org, id)
);
CREATE INDEX bug_report_person ON bug_report (tenant_id, org, reporter_id, created_at DESC, id DESC);
CREATE INDEX bug_report_inbox ON bug_report (tenant_id, org, status, updated_at, id);
CREATE TABLE bug_report_submission (
    tenant_id text NOT NULL, org text NOT NULL, report_id uuid NOT NULL,
    submission_rev bigint NOT NULL CHECK (submission_rev > 0),
    payload jsonb NOT NULL, payload_hash text NOT NULL,
    sink_profile_id text NOT NULL CHECK (sink_profile_id = 'disabled'), sink_config_version bigint NOT NULL,
    policy_rev bigint NOT NULL, consent_actor text NOT NULL, consent_at timestamptz NOT NULL DEFAULT now(),
    content_consent boolean NOT NULL, notice_version text NOT NULL,
    approved_by text, approved_at timestamptz, rejected_by text, rejected_at timestamptz, rejection_reason text,
    PRIMARY KEY (tenant_id, org, report_id, submission_rev),
    FOREIGN KEY (tenant_id, org, report_id) REFERENCES bug_report (tenant_id, org, id)
);
CREATE INDEX bug_report_submission_rate ON bug_report_submission (tenant_id, org, consent_actor, consent_at);
CREATE TABLE bug_report_delivery (
    tenant_id text NOT NULL, org text NOT NULL, report_id uuid NOT NULL, submission_rev bigint NOT NULL,
    id uuid NOT NULL UNIQUE,
    PRIMARY KEY (tenant_id, org, report_id, submission_rev),
    FOREIGN KEY (tenant_id, org, report_id, submission_rev)
      REFERENCES bug_report_submission (tenant_id, org, report_id, submission_rev)
);
CREATE TABLE bug_report_command (
    tenant_id text NOT NULL, org text NOT NULL, actor_id text NOT NULL,
    operation text NOT NULL, idempotency_key text NOT NULL, request_hash text NOT NULL,
    report_id uuid NOT NULL, result jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org, actor_id, operation, idempotency_key),
    FOREIGN KEY (tenant_id, org, report_id) REFERENCES bug_report (tenant_id, org, id)
);
ALTER TABLE bug_report_policy ENABLE ROW LEVEL SECURITY;
ALTER TABLE bug_report_policy FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON bug_report_policy USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE bug_report ENABLE ROW LEVEL SECURITY;
ALTER TABLE bug_report FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON bug_report USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE bug_report_submission ENABLE ROW LEVEL SECURITY;
ALTER TABLE bug_report_submission FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON bug_report_submission USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE bug_report_delivery ENABLE ROW LEVEL SECURITY;
ALTER TABLE bug_report_delivery FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON bug_report_delivery USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));
ALTER TABLE bug_report_command ENABLE ROW LEVEL SECURITY;
ALTER TABLE bug_report_command FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON bug_report_command USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));

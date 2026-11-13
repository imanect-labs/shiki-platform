-- 本人の help-first 証拠。本文・検索クエリは保存しない。
CREATE TABLE help_resolution_case (
    tenant_id text NOT NULL, org text NOT NULL, id uuid NOT NULL,
    reporter_id text NOT NULL, thread_id uuid NOT NULL,
    guidance_message_id uuid NOT NULL, run_id uuid NOT NULL,
    help_tool_call_id text NOT NULL, fencing_token bigint NOT NULL, receipt jsonb NOT NULL,
    resolution text NOT NULL DEFAULT 'unknown' CHECK (resolution IN ('unknown','unresolved','resolved')),
    confirmed_at timestamptz, report_intent_at timestamptz,
    report_id uuid, draft_run_id uuid,
    expires_at timestamptz NOT NULL DEFAULT (now() + interval '24 hours'),
    PRIMARY KEY (tenant_id, org, id),
    UNIQUE (tenant_id, org, run_id, help_tool_call_id),
    FOREIGN KEY (tenant_id, org, report_id) REFERENCES bug_report (tenant_id, org, id)
);
CREATE INDEX help_case_person ON help_resolution_case (tenant_id,org,reporter_id,thread_id,guidance_message_id);
ALTER TABLE help_resolution_case ENABLE ROW LEVEL SECURITY;
ALTER TABLE help_resolution_case FORCE ROW LEVEL SECURITY;
CREATE POLICY shiki_tenant_isolation ON help_resolution_case
 USING (tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK (tenant_id = (SELECT shiki_rls_tenant()));

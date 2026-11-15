-- 管理画面のロール定義・IdPグループ対応表。所属の根拠は検証済みIdP claims。
ALTER TABLE directory_role ADD CONSTRAINT directory_role_org_identity UNIQUE (tenant_id, org, role_id);
CREATE TABLE directory_managed_role (
    tenant_id text NOT NULL,
    org text NOT NULL,
    role_id text NOT NULL,
    label text NOT NULL CHECK (char_length(label) BETWEEN 1 AND 80),
    parent_id text,
    keycloak_group_id text,
    version bigint NOT NULL DEFAULT 1,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org, role_id),
    FOREIGN KEY (tenant_id, org, role_id) REFERENCES directory_role (tenant_id, org, role_id),
    FOREIGN KEY (tenant_id, org, parent_id) REFERENCES directory_managed_role (tenant_id, org, role_id),
    CHECK (parent_id IS NULL OR parent_id <> role_id)
);
CREATE UNIQUE INDEX directory_managed_role_sibling_name
    ON directory_managed_role (tenant_id, org, coalesce(parent_id, ''), lower(label));
CREATE INDEX directory_managed_role_children ON directory_managed_role (tenant_id, org, parent_id, label, role_id);
CREATE TABLE directory_ad_group (
    tenant_id text NOT NULL,
    org text NOT NULL,
    group_id text NOT NULL,
    name text NOT NULL,
    claim_path text NOT NULL,
    distinguished_name text,
    active boolean NOT NULL DEFAULT true,
    ou_path text[] NOT NULL DEFAULT '{}',
    mapping_version bigint NOT NULL DEFAULT 0,
    parent_ids text[] NOT NULL DEFAULT '{}',
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org, group_id),
    UNIQUE (tenant_id, org, claim_path) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE directory_ad_mapping (
    tenant_id text NOT NULL,
    org text NOT NULL,
    group_id text NOT NULL,
    role_id text NOT NULL,
    PRIMARY KEY (tenant_id, org, group_id, role_id),
    FOREIGN KEY (tenant_id, org, group_id) REFERENCES directory_ad_group (tenant_id, org, group_id),
    FOREIGN KEY (tenant_id, org, role_id) REFERENCES directory_role (tenant_id, org, role_id)
);
CREATE INDEX directory_ad_mapping_role ON directory_ad_mapping (tenant_id, org, role_id);
CREATE TABLE directory_admin_operation (
    tenant_id text NOT NULL,
    org text NOT NULL,
    operation_id uuid NOT NULL,
    actor_id text NOT NULL,
    trace_id text,
    idempotency_key uuid NOT NULL,
    request_hash text NOT NULL,
    request jsonb NOT NULL,
    state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'running', 'succeeded', 'failed')),
    attempts integer NOT NULL DEFAULT 0,
    processed integer NOT NULL DEFAULT 0,
    total integer NOT NULL,
    error_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, org, operation_id),
    UNIQUE (tenant_id, org, idempotency_key)
);
CREATE INDEX directory_admin_operation_history ON directory_admin_operation (tenant_id, org, created_at DESC);
CREATE INDEX directory_admin_operation_pending ON directory_admin_operation (state, updated_at) WHERE state IN ('pending', 'running');
DO $$
DECLARE table_name text;
BEGIN
  FOREACH table_name IN ARRAY ARRAY['directory_managed_role', 'directory_ad_group', 'directory_ad_mapping', 'directory_admin_operation'] LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', table_name);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', table_name);
    EXECUTE format('CREATE POLICY shiki_tenant_isolation ON %I USING ((SELECT shiki_rls_tenant()) IS NULL OR tenant_id = (SELECT shiki_rls_tenant())) WITH CHECK ((SELECT shiki_rls_tenant()) IS NULL OR tenant_id = (SELECT shiki_rls_tenant()))', table_name);
  END LOOP;
END $$;

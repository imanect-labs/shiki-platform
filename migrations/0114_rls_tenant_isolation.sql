-- テナント表に RLS を入れる（#1067・RLS 段階導入の第 2 段・移行モード）。
--
-- 第 1 段（0113）で実行時の接続を非 superuser の shiki_runtime に分けた。ここでは DB 側の多層防御として、
-- テナント表に行レベルセキュリティ（RLS）を付ける。
--
-- # 移行モード
--
-- 本番コードの SQL はまだテナント文脈（`app.tenant_id`）を設定していない（約 920 か所。第 3 段で
-- クレートごとに `storage::db_tenant::begin_tenant_tx` へ移す）。そこで当面のポリシーは:
--
-- - 文脈が**未設定**なら通す（従来どおり。アプリの `WHERE tenant_id = …` だけが境界）。
-- - 文脈が**設定されていれば**、その `tenant_id` の行だけを読み書きできる（USING と WITH CHECK）。
--
-- 実行器へ移した経路から順に DB でも隔離が効く。全経路を移したら、未設定を拒否する厳格モードへ
-- 切り替える（`shiki_rls_tenant() IS NULL OR` を外す migration を足す）。
--
-- # 対象
--
-- `public` スキーマの通常表で、NOT NULL の `tenant_id` 列を持つもの（下の DO で導出する）。対象外:
--
-- - `data_*`: `tenant_int`・パーティション・プール分割の上にある（docs/data/shard-safety.md）。別の段で扱う。
-- - `tenant_id` が NULL を取り得る表（`outbox_delivery_seq` = consumer 単位の採番。NULL は横断の consumer）。
--
-- この migration より後にテナント表を足すときは、同じ ENABLE / FORCE / POLICY を付けること。付け忘れは
-- 結合テスト（crates/storage/tests/rls_it.rs）が落とす。

-- 現在のトランザクションのテナント（未設定なら NULL）。
--
-- 一度 `SET LOCAL app.tenant_id` した接続では、トランザクションの終わりに値が NULL ではなく空文字に
-- 戻る（カスタム GUC のプレースホルダが残るため）。空文字も「未設定」として扱う。
-- ポリシーでは `(SELECT shiki_rls_tenant())` とスカラ副問い合わせで包む。InitPlan になってクエリごとに
-- 1 回だけ評価される（包まないと行ごとに current_setting を呼び、全件走査が約 1.7 倍遅くなった）。
CREATE OR REPLACE FUNCTION shiki_rls_tenant()
RETURNS text
LANGUAGE sql
STABLE
PARALLEL SAFE
AS $$ SELECT NULLIF(pg_catalog.current_setting('app.tenant_id', true), '') $$;

GRANT EXECUTE ON FUNCTION shiki_rls_tenant() TO PUBLIC;

DO $$
DECLARE
    t record;
BEGIN
    FOR t IN
        SELECT c.relname
          FROM pg_catalog.pg_class c
          JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
          JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid
         WHERE n.nspname = 'public'
           AND c.relkind = 'r'
           AND NOT c.relispartition
           AND c.relname NOT LIKE 'data\_%'
           AND a.attname = 'tenant_id'
           AND a.attnotnull
           AND NOT a.attisdropped
         ORDER BY c.relname
    LOOP
        EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', t.relname);
        -- テーブル所有者（migration のロール）にも効かせる。superuser と BYPASSRLS のロールは対象外。
        EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY', t.relname);
        EXECUTE format('DROP POLICY IF EXISTS shiki_tenant_isolation ON public.%I', t.relname);
        EXECUTE format(
            'CREATE POLICY shiki_tenant_isolation ON public.%I '
            'USING ((SELECT shiki_rls_tenant()) IS NULL OR tenant_id = (SELECT shiki_rls_tenant())) '
            'WITH CHECK ((SELECT shiki_rls_tenant()) IS NULL OR tenant_id = (SELECT shiki_rls_tenant()))',
            t.relname
        );
    END LOOP;
END
$$;

-- テナント横断の保守処理（outbox のリレーと GC・期限切れの失効・運用 CLI 等）用のロール。
--
-- BYPASSRLS は継承されない属性なので、メンバになっただけでは効かない。横断用の実行器
-- （`storage::db_tenant::begin_cross_tenant_tx`）がトランザクションの中で `SET LOCAL ROLE` したときだけ効く。
-- shiki_runtime には INHERIT FALSE / SET TRUE で付与する（権限は継承せず、SET ROLE だけを許す）。
--
-- BYPASSRLS のロールを作るには superuser（PG16 では CREATEROLE かつ自身が BYPASSRLS）が要る。
-- migration の実行ロールにその権限が無い環境では、運用が先に作っておけばここは何もしない
-- （docs/guides/db-maintenance.md §4）。
DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'shiki_maintenance') THEN
        CREATE ROLE shiki_maintenance NOLOGIN NOSUPERUSER BYPASSRLS NOCREATEDB NOCREATEROLE;
    END IF;
END
$$;

GRANT USAGE ON SCHEMA public TO shiki_maintenance;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO shiki_maintenance;
GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO shiki_maintenance;
REVOKE ALL ON TABLE _sqlx_migrations FROM shiki_maintenance;
GRANT EXECUTE ON FUNCTION shiki_analyze(regclass, boolean) TO shiki_maintenance;
ALTER DEFAULT PRIVILEGES FOR ROLE CURRENT_USER IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO shiki_maintenance;
ALTER DEFAULT PRIVILEGES FOR ROLE CURRENT_USER IN SCHEMA public
    GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO shiki_maintenance;

GRANT shiki_maintenance TO shiki_runtime WITH INHERIT FALSE, SET TRUE;

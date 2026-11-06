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
-- `public` スキーマの通常表で `tenant_id` 列を持つもの（下の DO で導出する）。対象外は次の明示リストだけ:
--
-- - `data_*`: `tenant_int`・パーティション・プール分割の上にある（docs/data/shard-safety.md）。別の段で扱う。
-- - `outbox_delivery_seq`: `tenant_id` が NULL を取り得る（consumer 単位の採番。NULL は横断の consumer）。
-- - `audit_log`・`storage_event_outbox`: jsonb の式インデックス（`metadata ->> 'thread_id'` 等）と GIN（`@>`）
--   で引いている。RLS の付いた表では leakproof でない演算子をインデックス条件に使えないため、文脈が
--   未設定でもこれらのインデックスが効かなくなる（テナント内の全行走査に落ちる）。生成列＋btree に
--   置き換えてから対象に入れる。
--
-- この migration より後にテナント表を足すときは、同じ ENABLE / FORCE / POLICY を付けること。付け忘れと、
-- RLS 下で効かなくなるインデックスの追加は、結合テスト（crates/storage/tests/rls_it.rs）が落とす。
--
-- # ロック
--
-- ENABLE / FORCE ROW LEVEL SECURITY と CREATE POLICY は表ごとに ACCESS EXCLUSIVE ロックを取り、コミットまで
-- 持つ。長いクエリの後ろで待ち続けて全表を止めないよう、lock_timeout で打ち切る（起動時の migration が
-- 失敗するだけで、再起動で再試行される）。負荷の低い時間帯に適用すること（docs/guides/db-maintenance.md §5）。

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

SET LOCAL lock_timeout = '5s';

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
           AND c.relname NOT IN ('outbox_delivery_seq', 'audit_log', 'storage_event_outbox')
           AND a.attname = 'tenant_id'
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
-- （`storage::db_tenant::begin_cross_tenant_tx`）がトランザクションの中でロールを切り替えたときだけ効く。
-- shiki_runtime には INHERIT FALSE / SET TRUE で付与する（権限は継承せず、SET ROLE だけを許す）。
--
-- BYPASSRLS のロールを作るには superuser（PG16 では CREATEROLE かつ自身が BYPASSRLS）が要り、
-- shiki_runtime への付与にはこのロールの ADMIN が要る。migration の実行ロールにその権限が無い環境
-- （マネージド Postgres 等）では、NOTICE を出して作成・付与を飛ばす（migration は止めない）。
-- 移行モードのうちは横断処理が文脈を設定しないだけで動くので、ロールが無くても既存の経路は壊れない。
-- 運用は docs/guides/db-maintenance.md §5 の手順でロールと付与を用意する。
DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'shiki_maintenance') THEN
        BEGIN
            CREATE ROLE shiki_maintenance NOLOGIN NOSUPERUSER BYPASSRLS NOCREATEDB NOCREATEROLE;
        EXCEPTION
            WHEN insufficient_privilege THEN
                RAISE NOTICE 'shiki_maintenance を作る権限がありません。運用が作成してください（db-maintenance.md §5）';
                RETURN;
            -- 新規のクラスタで複数の migration が同時に走ったとき（結合テスト等）。
            WHEN duplicate_object OR unique_violation THEN
                NULL;
        END;
    END IF;
    IF NOT (SELECT rolbypassrls FROM pg_catalog.pg_roles WHERE rolname = 'shiki_maintenance') THEN
        RAISE WARNING 'shiki_maintenance が BYPASSRLS を持っていません。テナント横断の処理が RLS で絞られます';
    END IF;

    GRANT USAGE ON SCHEMA public TO shiki_maintenance;
    GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO shiki_maintenance;
    GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO shiki_maintenance;
    REVOKE ALL ON TABLE _sqlx_migrations FROM shiki_maintenance;
    GRANT EXECUTE ON FUNCTION shiki_analyze(regclass, boolean) TO shiki_maintenance;
    ALTER DEFAULT PRIVILEGES FOR ROLE CURRENT_USER IN SCHEMA public
        GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO shiki_maintenance;
    ALTER DEFAULT PRIVILEGES FOR ROLE CURRENT_USER IN SCHEMA public
        GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO shiki_maintenance;

    BEGIN
        GRANT shiki_maintenance TO shiki_runtime WITH INHERIT FALSE, SET TRUE;
    EXCEPTION
        WHEN insufficient_privilege THEN
            RAISE NOTICE 'shiki_maintenance を shiki_runtime に付与する権限がありません。運用が付与してください（db-maintenance.md §5）';
    END;
END
$$;

-- 実行時ロール（shiki_runtime）を DDL の所有者から分ける（#1030・RLS 段階導入の第 1 段）。
--
-- これまでアプリは superuser の接続で migration も実行時のクエリも流していた。superuser と
-- テーブル所有者は RLS を迂回するので、RLS を入れても効かない。実行時は DML だけを持つ
-- 非 superuser のロールで接続し、migration（DDL）だけを所有者の接続で流す。
--
-- shiki_runtime はグループロール（NOLOGIN）。ログイン用のロール（compose では shiki_app）は
-- デプロイ側で作り、このロールのメンバにする（パスワードを migration に書かないため）:
--   CREATE ROLE shiki_app LOGIN PASSWORD '...' IN ROLE shiki_runtime;
-- CREATE ROLE には CREATEROLE 権限が要る。migration の実行ユーザにその権限が無い環境では、
-- 運用が先に shiki_runtime を作っておけば、ここは何もしない（IF NOT EXISTS）。
DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'shiki_runtime') THEN
        BEGIN
            CREATE ROLE shiki_runtime NOLOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE;
        EXCEPTION
            -- ロールはクラスタ共通なので、新規のクラスタで複数の DB の migration が同時に走ると
            -- （結合テストが DB を作り分けるとき等）、存在確認の後に別の接続が先に作ることがある。
            WHEN duplicate_object OR unique_violation THEN
                NULL;
        END;
    END IF;
END
$$;

-- 既存のオブジェクト（DML だけ。TRUNCATE・REFERENCES・TRIGGER は渡さない）。
GRANT USAGE ON SCHEMA public TO shiki_runtime;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO shiki_runtime;
GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO shiki_runtime;
-- migration の台帳は実行時ロールに触らせない（行を消す・偽の版を足すと次の起動で migration が誤動作する）。
REVOKE ALL ON TABLE _sqlx_migrations FROM shiki_runtime;

-- これ以降の migration で増えるオブジェクトにも同じ権限を付ける。既定権限は「作ったロール」ごとに
-- 効くので、0113 を流したロール（current_user）を明示する。migration の所有者ロールを替えたら、
-- 新しい所有者で同じ ALTER DEFAULT PRIVILEGES FOR ROLE を流すこと（docs/guides/db-maintenance.md §4）。
ALTER DEFAULT PRIVILEGES FOR ROLE CURRENT_USER IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO shiki_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE CURRENT_USER IN SCHEMA public
    GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO shiki_runtime;

-- 構造化データのパーティションの統計を取り直す入口（#986・PIT-69）。
--
-- ANALYZE は（PG16 では）テーブル所有者か superuser にしかできない。実行時ロールにテーブルの
-- 所有権を渡すと RLS を迂回できてしまうので、対象を data_* の表に絞った SECURITY DEFINER 関数に
-- してこれだけを実行時ロールに許す。
CREATE OR REPLACE FUNCTION shiki_analyze(rel regclass, skip_locked boolean DEFAULT false)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
-- pg_temp を末尾に明示する。省くと pg_temp が先頭で探され、呼び出し側が一時ビュー等で pg_class を
-- 覆い隠して、定義者（所有者）の権限で任意の関数を走らせられる（SECURITY DEFINER の定石）。
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    target record;
BEGIN
    SELECT n.nspname, c.relname
      INTO target
      FROM pg_catalog.pg_class c
      JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
     WHERE c.oid = rel;
    IF target.nspname IS DISTINCT FROM 'public' OR target.relname NOT LIKE 'data\_%' THEN
        RAISE EXCEPTION 'shiki_analyze は public.data_* の表だけを対象にします: %', rel::text
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF skip_locked THEN
        EXECUTE format('ANALYZE (SKIP_LOCKED) %I.%I', target.nspname, target.relname);
    ELSE
        EXECUTE format('ANALYZE %I.%I', target.nspname, target.relname);
    END IF;
END
$$;

REVOKE ALL ON FUNCTION shiki_analyze(regclass, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION shiki_analyze(regclass, boolean) TO shiki_runtime;

-- 組織の member と部署の role#member を Keycloak の所属グループと定期に照合する（D-136）続きの位置。
--
-- 照合は、ログインの同期が載せた人（directory_user）を (tenant_id, user_id) の順に 1 回に決まった人数ずつ読む。
-- サーバーが何台あっても 1 台だけが進め（advisory lock）、次の回はどのサーバーでもここから続ける。最後まで読んだら
-- 行を消して頭から読み直す（行が無い＝頭から）。
--
--   tenant_id, user_id = 最後に照合を終えた人。

create table membership_reconcile_cursor (
    id         smallint    primary key default 1 check (id = 1),
    tenant_id  text        not null,
    user_id    text        not null,
    updated_at timestamptz not null default now()
);

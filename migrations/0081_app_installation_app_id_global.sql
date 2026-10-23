-- #817: B1 配信のテナント跨ぎ解決（resolve_active_by_app_global）の前提を DB で保証する。
--
-- B1 第3リスナ（`GET /a/{app_id}/{sha256}`）は cookie も URL 上のテナントも持たない（opaque origin・
-- cookieless 配信）ため、`app_id` だけから active インストールと所属テナントを引く。`app_id` は
-- mini_app_code アーティファクトの id（同一テナント内で gen_random_uuid() 採番）で実質大域一意だが、
-- 既存の制約は `unique (tenant_id, app_id)` のテナント内一意しか無く、仮に 2 テナントで同じ
-- app_id が active になると解決先が不定（LIMIT 1 の任意行）になっていた。
--
-- active な行に限って app_id を大域一意にする（revoked 行は監査用に残るため対象外）。重複は
-- インストール（upsert）時点の一意制約違反で fail-closed に弾かれ、配信時の曖昧さは構造的に消える。
-- 既存データは gen_random_uuid() 由来の app_id しか持たないため、この索引の作成は失敗しない想定。
create unique index app_installation_active_app_id_uidx
    on app_installation (app_id)
    where status = 'active';

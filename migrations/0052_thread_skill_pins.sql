-- Phase 10 Task 10.11（#344）: skill ピンの複数化。
--
-- thread の 1 対列（0029）は「1 スレッド 1 スキル・作成時固定」を構造で強制していた。
-- skill をエージェントループに載せる（カタログ引きツール化）に伴い、ピンは
-- 「最初からロード済みのスキル」の集合（順序付き・編集可）へ一般化する。
--
-- - thread 側: 正規化テーブル `thread_skill_pin`（変更 API / UI 一覧の対象）。
-- - generation_run 側: post 時点のピンの **jsonb スナップショット**（run 行が生成材料の
--   単一ソース、という 0027/0029 の原則を維持。claim が 1 行で完結する）。
-- - mini_app の 1 対列は据え置き（バンドル経由セッションの意味は変わらない）。

create table thread_skill_pin (
    thread_id     uuid   not null references thread(id) on delete cascade,
    tenant_id     text   not null,
    skill_id      uuid   not null,
    skill_version bigint not null,
    -- カタログ/適用の順序（apply は position 順・後勝ちのモデル既定に影響する）。
    position      int    not null default 0,
    primary key (thread_id, skill_id)
);

-- 既存の 1 対ピンを移行する（version は CHECK で対が保証されている）。
insert into thread_skill_pin (thread_id, tenant_id, skill_id, skill_version)
    select id, tenant_id, skill_id, skill_version from thread where skill_id is not null;

alter table thread drop constraint thread_skill_pin_chk;
alter table thread drop column skill_id, drop column skill_version;

-- run 側はスナップショット列（[{"skill_id": ..., "skill_version": ...}, ...]）。
alter table generation_run add column skill_pins jsonb not null default '[]'::jsonb;

update generation_run
    set skill_pins = jsonb_build_array(
        jsonb_build_object('skill_id', skill_id, 'skill_version', skill_version))
    where skill_id is not null;

alter table generation_run drop constraint run_skill_pin_chk;
alter table generation_run drop column skill_id, drop column skill_version;

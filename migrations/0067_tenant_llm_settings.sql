-- テナントの AI 設定（モデルカタログの絞り込み・既定・Auto）と、利用量の利用元・
-- チャット run ごとのモデル指定。
--
-- モデルとプロバイダの実体（接続先・API キー・単価）はデプロイの設定が正（秘密を DB に置かない）。
-- ここはテナント管理者が「その中から何を使わせるか」を決める層だけを持つ。行が無いテナントは
-- 全モデルを使え、既定は Auto（ユーザーがモデルを選ばない）。

create table tenant_llm_settings (
    tenant_id          text        primary key,
    -- 新しいチャットの既定。'auto' かカタログのモデル ID。
    default_model      text        not null default 'auto',
    -- ユーザーがチャットでモデルを選べるか（false なら全員が既定で使う）。
    allow_user_choice  boolean     not null default true,
    -- ユーザーがモデルを選んだときの思考強度の初期値。
    default_effort     text        not null default 'medium'
                       check (default_effort in ('low', 'medium', 'high')),
    -- Auto の振り先の上書き（NULL はデプロイ設定 `llm.auto`）。
    auto_model         text,
    auto_effort        text        check (auto_effort in ('low', 'medium', 'high')),
    -- 利用量の円換算レート（1 USD あたりの円 × 100）。NULL は円を出さない。参考表示のみで、
    -- 会計（llm_usage.cost_usd_micros）は USD のまま持つ。
    jpy_per_usd_centi  integer     check (jpy_per_usd_centi between 100 and 100000),
    updated_by         text,
    updated_at         timestamptz not null default now()
);

-- テナントが無効にしたモデル（行が無いモデルは有効）。デプロイ設定に新しいモデルを足したとき、
-- 既存テナントに行を作らなくても使えるよう「無効の記録」で持つ。
create table tenant_llm_model (
    tenant_id   text        not null,
    model_id    text        not null,
    enabled     boolean     not null,
    updated_by  text,
    updated_at  timestamptz not null default now(),
    primary key (tenant_id, model_id)
);

-- 利用量の利用元（管理画面の利用元別集計）。既存行はミニアプリ経由（app_id あり）だけ判別できる。
alter table llm_usage
    add column source text not null default 'other'
        check (source in ('chat', 'agent', 'workflow', 'miniapp', 'other'));
update llm_usage set source = 'miniapp' where app_id is not null;

-- チャット run のモデル指定（投入時点でスナップショット・承認モードと同じ扱い）。
-- model が NULL の run は従来どおり（skill のピン → 既定モデル）。model_auto はユーザーが
-- 選ばなかった（Auto）ことを示し、そのときだけ skill のピンがモデルを上書きできる。
alter table generation_run
    add column model      text,
    add column effort     text check (effort in ('low', 'medium', 'high')),
    add column model_auto boolean not null default true;

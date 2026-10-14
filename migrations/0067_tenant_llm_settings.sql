-- 組織の AI 設定（モデルカタログの絞り込み・既定・Auto）と、利用量の利用元・
-- チャット run ごとのモデル指定。
--
-- モデルとプロバイダの実体（接続先・API キー・単価）はデプロイの設定が正（秘密を DB に置かない）。
-- ここは組織の管理者が「その中から何を使わせるか」を決める層だけを持つ。行が無い組織は
-- カタログの既定のモデルを使え、既定は Auto（ユーザーがモデルを選ばない）。
--
-- 粒度は (tenant_id, org): 変更できるのは organization:<tenant>|<org>#admin で、利用量の集計も
-- tenant_id ＋ org に閉じる。テナント単位にすると、ある org の管理者が同じテナントの別 org の
-- 設定まで変えられてしまう。

create table tenant_llm_settings (
    tenant_id          text        not null,
    org                text        not null,
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
    updated_at         timestamptz not null default now(),
    primary key (tenant_id, org)
);

-- 組織が許可したモデル（許可リスト）。設定の行（tenant_llm_settings）が無い組織は未保存で、
-- カタログの既定（requires_opt_in でないモデル）を使える。管理者が保存した時点で許可リストが
-- 確定し、それ以降にデプロイ設定へ足したモデルは管理者が有効にするまで使えない。
create table tenant_llm_model (
    tenant_id   text        not null,
    org         text        not null,
    model_id    text        not null,
    updated_by  text,
    updated_at  timestamptz not null default now(),
    primary key (tenant_id, org, model_id)
);

-- 利用量の利用元（管理画面の利用元別集計）。既存行はミニアプリ経由（app_id あり）だけ判別できる。
-- 定数 default の列追加はメタデータだけで済む。CHECK は NOT VALID で付けて既存行の全件検証を
-- 避け（新しい行は検証される）、会計の INSERT を長く止めない。
alter table llm_usage
    add column source text not null default 'other';
alter table llm_usage
    add constraint llm_usage_source_check
        check (source in ('chat', 'agent', 'workflow', 'miniapp', 'other')) not valid;
-- 既存行のバックフィル（app_id あり → miniapp）は 0068 に分ける。ここで流すと、上の ALTER が取った
-- ACCESS EXCLUSIVE ロックをトランザクション終了まで握ったまま全件更新することになる。

-- チャット run のモデル指定（投入時点でスナップショット・承認モードと同じ扱い）。
-- model が NULL の run は従来どおり（skill のピン → 既定モデル）。model_auto はユーザーが
-- 選ばなかった（Auto）ことを示し、そのときだけ skill のピンがモデルを上書きできる。
alter table generation_run
    add column model      text,
    add column effort     text check (effort in ('low', 'medium', 'high')),
    add column model_auto boolean not null default true;

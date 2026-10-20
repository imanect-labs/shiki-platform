-- AI 活用分析（#751）: 従業員がどれだけ・何に AI を使い、何が良くなったかを見るための集計と収集。
--
-- 粒度はすべて (tenant_id, org)。管理 API（organization#admin）と利用量の集計と同じ単位。
-- 管理者は会話本文を読めない（PIT-65）。ここに本文は置かない（用途はラベル、評価はカテゴリだけ）。

-- 利用者の部署（ロール）の写し。正本は IdP claims → OpenFGA の role タプル（ログイン時に同期）で、
-- 集計のたびに OpenFGA を引かないよう（PIT-18）、同期と同じ集合をここにも書く。
-- source: 'group' = groups claim 由来の部署 / 'role' = roles claim 由来のロール。
create table directory_user_role (
    tenant_id   text        not null,
    org         text        not null,
    user_id     text        not null,
    role_id     text        not null,
    source      text        not null check (source in ('group', 'role')),
    updated_at  timestamptz not null default now(),
    primary key (tenant_id, user_id, role_id)
);

create index directory_user_role_org_idx on directory_user_role (tenant_id, org, role_id);

-- 回答への評価（FR-16 の既定＝メタデータのみ。本文・自由記述は持たない＝スキーマ上不能）。
-- 1 人 1 回答 1 行。付け直しは上書き、取り消しは行の削除。
create table message_feedback (
    tenant_id   text        not null,
    org         text        not null,
    message_id  uuid        not null references message (id) on delete cascade,
    thread_id   uuid        not null references thread (id) on delete cascade,
    user_id     text        not null,
    rating      smallint    not null check (rating in (-1, 1)),
    -- 👎 の理由（👍 では NULL）。
    category    text        check (category in
                    ('inaccurate', 'not_found', 'not_followed', 'slow', 'how_to', 'other')),
    -- 評価した回答を作ったモデル（run のスナップショット・モデル別満足度の集計用）。
    model       text,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now(),
    primary key (tenant_id, message_id, user_id),
    check (rating = -1 or category is null)
);

create index message_feedback_org_idx on message_feedback (tenant_id, org, created_at);

-- 会話の用途ラベル（夜間に AI が分類・本文は保存しない）。分類は組織の設定でオンのときだけ。
-- messages_seen: 分類に使った時点のユーザー発話数（増えたら分類し直す）。
create table thread_insight (
    tenant_id      text        not null,
    org            text        not null,
    thread_id      uuid        not null references thread (id) on delete cascade,
    owner          text        not null,
    category       text        not null check (category in
                       ('writing', 'summarize', 'research', 'internal_search', 'data_analysis',
                        'translation', 'ideation', 'coding', 'other')),
    messages_seen  integer     not null,
    model          text,
    classified_at  timestamptz not null default now(),
    primary key (tenant_id, thread_id)
);

create index thread_insight_org_idx on thread_insight (tenant_id, org, classified_at);

-- 利用者×日のロールアップ（日付は日本時間）。定期ジョブが日単位で作り直す（jobq / タイマーは
-- at-least-once なので増分では持たない・PIT-31）。画面は前日までをここから、当日分もここから読む
-- （ジョブが当日分も更新する）。
create table ai_activity_daily (
    tenant_id        text        not null,
    org              text        not null,
    day              date        not null,
    user_id          text        not null,
    -- llm_usage（利用元別の呼び出し回数・トークン・金額）
    calls            integer     not null default 0,
    chat_calls       integer     not null default 0,
    agent_calls      integer     not null default 0,
    workflow_calls   integer     not null default 0,
    miniapp_calls    integer     not null default 0,
    tokens           bigint      not null default 0,
    cost_usd_micros  bigint      not null default 0,
    -- 会話（その日に発話したスレッド数・ユーザー発話数）
    threads          integer     not null default 0,
    user_messages    integer     not null default 0,
    -- run（エージェントモード・失敗・中断）
    runs             integer     not null default 0,
    agent_runs       integer     not null default 0,
    failed_runs      integer     not null default 0,
    cancelled_runs   integer     not null default 0,
    -- 機能（回答のツール呼び出し・監査ログ・ワークフロー）
    doc_searches     integer     not null default 0,
    web_searches     integer     not null default 0,
    code_runs        integer     not null default 0,
    documents        integer     not null default 0,
    skill_uses       integer     not null default 0,
    miniapp_uses     integer     not null default 0,
    workflow_runs    integer     not null default 0,
    workflow_ok      integer     not null default 0,
    computed_at      timestamptz not null default now(),
    primary key (tenant_id, org, day, user_id)
);

-- どこまで集計したか（組織ごと）。ジョブは前日・当日を毎回作り直し、未集計の過去を少しずつ埋める。
create table ai_insight_rollup_state (
    tenant_id        text        not null,
    org              text        not null,
    computed_through date        not null,
    updated_at       timestamptz not null default now(),
    primary key (tenant_id, org)
);

-- 組織の分析設定。行が無ければ既定（分類オフ・個人別オフ・前提値は既定）。
create table ai_insight_settings (
    tenant_id          text        not null,
    org                text        not null,
    -- 会話の用途分類（本文を AI に渡す処理なので既定オフ）。
    classify_enabled   boolean     not null default false,
    -- 個人単位の表示（既定オフ。オンでも閲覧は監査ログに残す）。
    individual_view    boolean     not null default false,
    -- 金額換算の時給（円）。NULL は換算しない。
    hourly_wage_jpy    integer     check (hourly_wage_jpy between 1 and 100000),
    -- 用途ごとの「人がやると何分か」（カテゴリ → 分）。無いカテゴリは既定値。
    minutes_per_task   jsonb       not null default '{}'::jsonb,
    -- ワークフロー 1 回の成功で浮く分数。
    workflow_minutes   integer     not null default 3 check (workflow_minutes between 0 and 600),
    updated_by         text,
    updated_at         timestamptz not null default now(),
    primary key (tenant_id, org)
);

-- 日次集計が監査ログから機能の利用（skill.apply / gateway.call）を数えるための部分索引。
-- 注: CONCURRENTLY にしない（0047 と同じ理由: 並列に流れる `sqlx::migrate!` の advisory lock と
-- デッドロックする。この PR の結合テストでも再現した）。対象 action だけの部分索引なので索引は
-- 小さいが、構築中は audit_log への書き込みが待たされる（大きな既存環境では作成時間に注意）。
create index if not exists audit_log_insight_idx
    on audit_log (tenant_id, org, created_at)
    where action in ('skill.apply', 'gateway.call');

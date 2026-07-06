-- Phase 3: チャットドメイン（Task 3.1 / 3.11）。
--
-- thread / message は会話の永続表現。content は構造化ブロック配列（JSONB）で、
-- 添付はストレージ node の参照のみ持つ（実体二重持ち無し）。parent_id はブランチ可能
-- 構造だが UI は線形取得する。agent_mode は thread 既定＋message 単位で上書き可
-- （OFF=古典RAG注入＋llm-gateway 直 / ON=agent-core ツールループ）。
--
-- generation_run / generation_event は「接続非依存生成」（Task 3.11・design §4.4.1）の土台。
--   * 投入: POST /threads/:id/messages が単一 Tx で user/assistant message 保存＋run 行
--     ＋jobq enqueue（outbox 型）。202 で run_id を即返す。
--   * 実行: 生成ワーカーが run を claim（queued→running・lease_until/worker_id/fencing_token）。
--   * イベント: generation_event(run_id, seq) を単調 seq で append＝真実のソース。
--     message.content はその projection。(run_id, seq) unique で追記 exactly-once。
--   * 購読: GET /threads/:id/stream が Last-Event-ID(seq) 以降を DB replay ＋Redis 購読で配信。
--
-- 規約: 全行 tenant_id not null。SaaS 共用プールでも越境しないよう org+tenant で常にスコープする。
-- テナント消去（SAAS.2）は thread を tenant_id で削除すれば message→run→event へ CASCADE する。

create table thread (
    id           uuid        primary key default gen_random_uuid(),
    org          text        not null,
    tenant_id    text        not null,
    -- 作成者（subject の local id ＝ principal.id）。OpenFGA の owner タプルが権限の正本。
    owner        text        not null,
    title        text        not null,
    -- thread 既定のエージェントモード（message 単位で上書き可）。既定 OFF＝通常チャット。
    agent_mode   boolean     not null default false,
    -- 論理削除（一覧から除外）。
    deleted_at   timestamptz,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);

-- 自分のスレッド一覧（更新日降順・org+tenant スコープ・生存のみ）。
create index thread_owner_idx
    on thread (tenant_id, org, owner, updated_at desc)
    where deleted_at is null;

create table message (
    id           uuid        primary key default gen_random_uuid(),
    thread_id    uuid        not null references thread (id) on delete cascade,
    org          text        not null,
    tenant_id    text        not null,
    role         text        not null check (role in ('user', 'assistant', 'system', 'tool')),
    -- ブランチ可能構造（同一 thread 内の親メッセージ）。UI は線形取得。
    parent_id    uuid        references message (id) on delete set null,
    -- content = 構造化ブロック配列（text/thinking/tool_call/tool_result/citation/generative_ui/file_ref）。
    content      jsonb       not null default '[]'::jsonb,
    -- このメッセージ生成時のエージェントモード（thread 既定を上書きした実値）。
    agent_mode   boolean     not null default false,
    created_at   timestamptz not null default now()
);

-- スレッド内メッセージの線形取得（作成順）。
create index message_thread_idx on message (thread_id, created_at, id);

create table generation_run (
    run_id           uuid        primary key default gen_random_uuid(),
    -- 生成対象の assistant message（projection 先）。
    message_id       uuid        not null references message (id) on delete cascade,
    thread_id        uuid        not null references thread (id) on delete cascade,
    org              text        not null,
    tenant_id        text        not null,
    -- 発話ユーザーの subject local id（principal.id）。ワーカーは**この本人の権限で生成**し
    -- 昇格しない（confused-deputy 防御）。ワーカーが AuthContext を再構築する材料。
    actor            text        not null,
    status           text        not null default 'queued'
                     check (status in ('queued', 'running', 'done', 'failed', 'cancelled')),
    -- このメッセージ生成の実効エージェントモード。
    agent_mode       boolean     not null default false,
    -- リース: 保持ワーカーのみが単一ライタ。失効で別ワーカーが takeover 可能。
    lease_until      timestamptz,
    worker_id        text,
    -- クラッシュ takeover のフェンシング。claim ごとに +1 し、旧ワーカーのゾンビ書込を拒否する。
    fencing_token    bigint      not null default 0,
    -- 協調キャンセル: ユーザー明示停止でのみ true。ページ離脱≠キャンセル。
    cancel_requested boolean     not null default false,
    -- pgmq 側の配信試行と別に、run 単位の実行試行回数（可視化・DLQ 判定補助）。
    attempt          int         not null default 0,
    last_error       text,
    created_at       timestamptz not null default now(),
    updated_at       timestamptz not null default now()
);

-- 孤児回収 sweeper: 実行中でリース失効した run を拾う走査経路。
create index generation_run_lease_idx
    on generation_run (lease_until)
    where status = 'running';

-- thread 再訪時の最新 run 特定（途中経過/状態の復元表示）。
create index generation_run_message_idx on generation_run (message_id);

create table generation_event (
    run_id       uuid        not null references generation_run (run_id) on delete cascade,
    -- run ごと単調増加の seq。真実のソースの追記順序。
    seq          bigint      not null,
    -- token / thinking / tool_call / tool_result / citation / generative_ui / error / done ...
    type         text        not null,
    payload      jsonb       not null default '{}'::jsonb,
    created_at   timestamptz not null default now(),
    -- (run_id, seq) unique で追記を exactly-once に潰す（重複 seq の二重書込を拒否）。
    primary key (run_id, seq)
);

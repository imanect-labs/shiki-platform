-- Task 9.12: B2 ランタイム（サーバ関数・event/cron トリガ）のピンとスケジュール（PR11）。

-- サーバコードバンドルの同意時ピン（frontend_bundle と同じ content address 原則）。
ALTER TABLE app_installation
    ADD COLUMN server_bundle TEXT,
    -- ServerSpec 全体の同意時ピン（functions/egress_allowlist/events/cron・JSONB）。
    -- 実行時の関数宣言・egress 判定・トリガ突合はこのピンだけを見る。
    ADD COLUMN server_spec JSONB;

-- イベント購読（インストール時に manifest server.events から実体化・アンインストールで削除）。
-- outbox 配送台帳コンシューマ "miniapp-functions" がここと突合して関数を起動する。
CREATE TABLE app_event_subscription (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  TEXT        NOT NULL,
    org        TEXT        NOT NULL,
    app_id     UUID        NOT NULL,
    -- 購読するイベント種別（例 data.record.transitioned・app.installed）。
    event_type TEXT        NOT NULL,
    -- 起動する関数名（manifest server.functions 宣言内）。
    function   TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, app_id, event_type, function)
);

CREATE INDEX idx_app_event_subscription_type
    ON app_event_subscription (tenant_id, event_type);

-- cron スケジュール（インストール時に manifest server.cron から実体化）。
-- リーダー（advisory lock）が due を拾い、(schedule_id, scheduled_at) 一意で二重起動を防ぐ。
CREATE TABLE app_function_schedule (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   TEXT        NOT NULL,
    org         TEXT        NOT NULL,
    app_id      UUID        NOT NULL,
    function    TEXT        NOT NULL,
    -- cron 式（5 フィールド・分解能は分）。
    expr        TEXT        NOT NULL,
    next_run_at TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, app_id, function, expr)
);

CREATE INDEX idx_app_function_schedule_due
    ON app_function_schedule (next_run_at);

-- 実行台帳（(schedule_id, scheduled_at) 一意＝リーダー交代/再起動でも同一時刻の二重起動なし）。
CREATE TABLE app_function_run (
    id           BIGSERIAL   PRIMARY KEY,
    schedule_id  UUID        NOT NULL REFERENCES app_function_schedule (id) ON DELETE CASCADE,
    scheduled_at TIMESTAMPTZ NOT NULL,
    started_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (schedule_id, scheduled_at)
);

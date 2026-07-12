-- Task 9.8: 能力アダプタの利用量計上と通知（PR7）。
--
-- app_capability_usage: ゲートウェイ能力呼び出しの (ユーザー×アプリ×能力×日) 集計。
-- dual_gate middleware が成功応答時に upsert する（コスト按分・監査は audit_log が正）。
CREATE TABLE app_capability_usage (
    tenant_id  TEXT        NOT NULL,
    org        TEXT        NOT NULL,
    app_id     UUID        NOT NULL,
    user_sub   TEXT        NOT NULL,
    -- CapabilityScope の文字列表現（"data.read" 等・閉集合は Rust enum が正）。
    capability TEXT        NOT NULL,
    day        DATE        NOT NULL,
    calls      BIGINT      NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, app_id, user_sub, capability, day)
);

-- アプリ別の利用量参照（GET /apps/{id}/usage・PR12）向け。
CREATE INDEX idx_app_capability_usage_app
    ON app_capability_usage (tenant_id, app_id, day);

-- app_notification: notify.send の永続先（アプリ→ユーザー通知）。
-- 配信 UI（web の通知一覧）は後続 PR。ここでは記録と監査のみ。
CREATE TABLE app_notification (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  TEXT        NOT NULL,
    org        TEXT        NOT NULL,
    app_id     UUID        NOT NULL,
    -- 宛先ユーザー（principal id・OIDC sub）。
    recipient  TEXT        NOT NULL,
    title      TEXT        NOT NULL,
    body       TEXT,
    -- 送信主体（呼出ユーザーの sub。アプリは confused-deputy 防御でユーザー代理のみ）。
    created_by TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    read_at    TIMESTAMPTZ
);

-- 受信者の未読一覧（新しい順）向け。
CREATE INDEX idx_app_notification_recipient
    ON app_notification (tenant_id, recipient, created_at DESC);

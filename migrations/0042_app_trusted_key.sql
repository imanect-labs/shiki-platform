-- Task 9.13b: 信頼鍵台帳（PR9）。
--
-- first-party アプリの publish/インストール、およびオフライン（エアギャップ）import の
-- 署名検証に使う ed25519 公開鍵。鍵の登録/失効は /admin 面（provisioner Bearer）のみ。
-- 秘密鍵はサーバに置かない（署名は CLI/CI 側・Task 9.14）。
CREATE TABLE app_trusted_key (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  TEXT        NOT NULL,
    org        TEXT        NOT NULL,
    -- 人間可読な鍵識別子（署名者が指定・import 時に照合）。
    key_id     TEXT        NOT NULL,
    -- ed25519 公開鍵（32 バイト raw）。
    public_key BYTEA       NOT NULL,
    note       TEXT,
    created_by TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- 失効（行は残す＝どの鍵で何が入ったかの監査可能性を保つ）。
    revoked_at TIMESTAMPTZ,
    UNIQUE (tenant_id, key_id)
);

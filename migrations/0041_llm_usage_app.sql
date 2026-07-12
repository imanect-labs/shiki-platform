-- Task 9.9: ミニアプリ内 AI のコスト計上と予算ピン（PR8）。
--
-- llm_usage: (ユーザー×アプリ) の集計軸を追加する。app_id はゲートウェイ経由の
-- AI 呼び出しのみ設定（chat 等の既存経路は NULL のまま）。user_sub は以後の全記録で
-- AuthContext から設定する（既存行は NULL＝過去分は tenant/org 集計のみ）。
ALTER TABLE llm_usage
    ADD COLUMN app_id   UUID,
    ADD COLUMN user_sub TEXT;

-- アプリ別の日次予算チェック（created_at 範囲）と利用量 API 向け。
CREATE INDEX idx_llm_usage_app
    ON llm_usage (tenant_id, app_id, created_at DESC)
    WHERE app_id IS NOT NULL;

-- app_installation: AI ガードレールの同意時ピン（Task 9.13b の同意フローがマニフェスト
-- Budget/tools から焼き込む。実行時は registry/artifact を読まない＝同意した内容だけが効く）。
ALTER TABLE app_installation
    -- 使用可能モデル（空＝テナントカタログ全体を許可）。
    ADD COLUMN budget_models TEXT[] NOT NULL DEFAULT '{}',
    -- 日次コスト上限（マイクロ USD・NULL＝管理者キャップのみ）。
    ADD COLUMN budget_daily_usd_micros BIGINT,
    -- 1 回の呼び出しの最大トークン（NULL＝プロファイル既定）。
    ADD COLUMN budget_max_tokens BIGINT,
    -- agent.invoke で提示してよい宣言ツール（ToolName 閉集合へ実行時照合）。
    ADD COLUMN agent_tools TEXT[] NOT NULL DEFAULT '{}';

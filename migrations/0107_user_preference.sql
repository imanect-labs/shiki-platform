-- ユーザー設定（設定 → チャット／表示・#952）。
--
-- 送信キー・新しいチャットの既定（モデル・Web 検索・承認モード）・表示の好み（思考過程の展開・日時の形式）。
-- 1 人 1 行（tenant_id ＋ org ＋ user_id）。範囲は AuthContext から決め、本人以外は読み書きしない。
--
-- 値は jsonb 1 列にまとめ、型と検証は Rust 側（chat::preferences::UserPreferences）で閉じる。
-- 欠けた項目・未知の項目は既定値として読むため、設定項目を足すときに migration を要しない。
-- 既定の承認モードはホームの承認モードの選択欄の初期値（スレッドへはサーバが勝手に適用しない）。全自動（bypass）を
-- 既定にするときは保存時に org キャップ（tenant.allow_autonomous_bypass）を検査する（chat::store::preferences）。
create table user_preference (
    tenant_id   text        not null,
    org         text        not null,
    user_id     text        not null,
    prefs       jsonb       not null default '{}'::jsonb check (jsonb_typeof(prefs) = 'object'),
    updated_at  timestamptz not null default now(),
    primary key (tenant_id, org, user_id)
);

-- Task 9.11: B1 フロントバンドルの同意時ピン（PR10）。
--
-- インストール（同意）時にマニフェスト frontend.sha256 を installation 行へ焼き込む。
-- 第3リスナ（B1 配信）は installation だけを見て配信する（実行時に registry/artifact を
-- 読まない・AiPin と同じ「同意した内容だけが効く」原則）。content address なので
-- 新バージョン publish はアップグレード同意（再インストール）まで配信に影響しない。
ALTER TABLE app_installation
    -- 配信するバンドルの sha256（hex 64・NULL = フロントなし/B2 のみ）。
    ADD COLUMN frontend_bundle TEXT;

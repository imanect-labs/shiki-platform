-- B2 のサービスアカウントの利用者（Keycloak の sub）を、インストールに残す（BUG-183）。
--
-- B2 の自動実行（event・cron）は、B2 のクライアントの client_credentials のトークンでゲートウェイを呼ぶ。そのトークンの
-- sub は、Keycloak がクライアントごとに作るサービスアカウントの利用者の id で、azp は B2 の client_id。ゲートウェイは、
-- azp が有効なインストールの B2 で、sub がこの列と一致するトークンだけを、アプリの主体（miniapp:<tenant>|<app_id>）
-- として扱う。テナントと組織はトークンのクレームではなく、この行から決める。同じ B2 が交換した利用者のトークン
-- （sub は利用者）は一致しないので、利用者のまま扱う。
--
-- インストールのときと、起動時のクライアントの照合（oauth_client_spec が今の形と違う行・migration 0141）が書く。
-- 空（NULL）は、B2 が無いか、まだ照合していない（この migration より前のインストール）。

alter table app_installation add column b2_service_sub text;

-- ゲートウェイがトークンの sub から引く（利用者のトークンは一致しないので、すぐに外れる）。
create index app_installation_b2_service_sub_idx
    on app_installation (b2_service_sub)
    where b2_service_sub is not null and status = 'active';

-- アプリのクライアント（Keycloak）を、どの登録の形で揃えたかを残す（BUG-172）。
--
-- 前の版のインストールは、B1 のクライアントの戻り先に BFF のログインの戻り先を登録し、既定のスコープ（sub を載せる
-- basic）と tenant・groups のマッパーを付けていなかった。そのクライアントでは、アプリがゲートウェイを呼べない。
-- サーバーは起動時に、有効なインストールのうち、この列が今の登録の形の指紋（app_gateway::client_spec_fingerprint・
-- B1 の戻り先を含む）と違うもののクライアントを、今の形に揃えてから指紋を書く。インストールのときも書く。
--
-- 空（NULL）は、まだ揃えていない（この migration より前のインストール）。

alter table app_installation add column oauth_client_spec text;

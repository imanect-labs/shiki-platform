# first-party skill バンドル（10.15・#344）

公式提供の skill（外部連携は **http.request ラップの skill** として配布し、ネイティブコネクタは
作らない方針の実証・[miniapp-platform.md](../../docs/miniapp-platform.md) §2.4/§4）。

## 配布経路（エアギャップと同一・マイグレーションに業務コンテンツを埋めない）

1. 管理者が信頼鍵を登録する（`POST /admin/trusted-keys`・ミニアプリと同じ台帳）。
2. 署名対象は **name/version に束縛した signing digest** =
   `app_platform::registry_signing_digest(name, version, app_platform::value_digest(body))` へ
   ed25519 で署名する（秘密鍵はオフライン保持・サーバに置かない。署名ヘルパ: `app_platform::sign_digest`）。
   body だけの署名だと別名で再 import して公式スキルをスプーフィングできてしまうため name/version を織り込む。
3. `POST /skills/registry/import { name, version, body, signature_base64 }` —
   登録済み信頼鍵で **signing digest** を検証し、artifact 化 → **first-party** として publish される
   （同一 name+version の再 import は 409・不変）。
4. 各ユーザーは `POST /skills/installations { name }` で自分のカタログへ入れる
   （first-party は署名検証済みのため**個別共有・管理者の個別同意なしで利用可能**）。

## slack-notify

- 実体: `.shiki` script が `Shiki.http.request` で Slack Web API（`chat.postMessage`）を呼ぶ。
- 前提: シークレット `slack-bot-token`（Bot User OAuth Token）を**宛先束縛 `slack.com`** で登録し、
  実行主体（対話なら本人・スケジュールならワークフロープリンシパル）に `can_use` を付与すること。
- 使い方: ワークフローの skill ノード `skill:slack-notify@<version>`（入力 `{ channel, text }`）。
  declared_scopes に `http.egress` が必要（scope ceiling）。
- 防御: トークン平文は script に渡らない（ホスト側でヘッダ注入）。宛先束縛 × egress allowlist の
  AND を URL ホスト部リテラルで照合・リダイレクトは一律拒否。監査は status + host のみ（redact）。

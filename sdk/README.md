# shiki ミニアプリ SDK / CLI（Task 9.14）

- `app-sdk/`: 公開 API ゲートウェイの型付きクライアント（`ShikiGateway`）＋B1 ホスト支援 PKCE
  トークン供給（`hostAssistedToken`）＋SSE ヘルパ。ブラウザ（B1）とツール連携向け。
- `cli/`: `shiki app init | dev | publish`。init で雛形＋ed25519 鍵、dev/publish で esbuild により
  フロント（単一 HTML）/サーバ（単一 JS）を固め、sha256 をマニフェストへ焼き、レジストリへ登録
  （publish の署名は canonical manifest digest への ed25519・backend の信頼鍵で検証）。

型は backend（utoipa / ts-rs）を正とし、SDK は薄いラッパのみ（ゲートウェイ ApiDoc の
openapi-typescript 配布は後続で gen-api に統合）。

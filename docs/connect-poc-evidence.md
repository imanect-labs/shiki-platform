# Shiki Connect PoC 検証記録（設計の根拠）

> 評価対象: [PoC #1321](https://github.com/imanect-labs/shiki-platform/issues/1321)。実測記録日2026-11-18（JST、raw logはUTC）。
> 本文書化は2026-11-19。**この設計PRでは試験を再実行せず、PoCコード・テスト・raw log・画像・構成はマージしない。**
> [Connect設計](./shiki-connect.md) / [Ops設計](./shiki-ops.md) / [共通基盤](./connect-ops-foundation.md)。

## 1. 証跡の位置と強さ

元記録はPoC作業ツリーの`eval/connect-poc/results/`に保持されている。
このPRのmainにはそのディレクトリが存在せず、以下のファイル名はその元記録内の識別子であり、repo内リンクではない。
完全な実装/runnerは別のPoCレビュー対象とし、この文書だけから試験を再実行できるとは主張しない。
Issue #1321に結果の要約を記録しているが、Issueの文面や下記hashだけでは試験結果の独立追試・署名付きattestationの代わりにならない。

対象base commitは`b8a4847c6b1523e18e4fbe42f32bd1402abb2adf`、その上の**未コミットPoC変更**を含む。
base SHAだけで試験時の全ソースを再現できない。環境はLinux x86_64、Rust 1.96.0、redb 4.6.0固定、
Dockerの実Samba/ディスク上UNFS3 NFSv3、SMBnetFS/ラボ専用libnfs-FUSE、debugビルド、共有開発マシン。

## 2. 記録された結果と試験境界

| 元記録名 | 試験 | 成功件数 | 境界 |
| --- | --- | --- | --- |
| `ledger-tests.log` | redbのtransaction/ACK/束縛/故障 | 6 | 実redb・SIGKILL。電源断の証明ではない |
| `poc-unit-tests.log` | Linux FS・ローカル変更・NAS writer局所境界 | 18 | 実FS/redb。6 FS＋8 local mutation＋4 writer。同一項目を別集計で足さない |
| `e2e.json` | TLS・StorageService・FGA・Postgres | 36 | 実サービス。RAG未設定時の501確認を含み、36件の検索成功ではない |
| `load-nat.json` | 負荷・bridge NAT・切断/失効 | 5 | 実接続。企業WAN/Firewallではない |
| `rag-e2e.json` | 既存RAG＋原本変更 | 24 | 実parser・Ruri・reranker・Qdrant/Tantivy・FGA |
| `nas-e2e.json` | SMB/NFSv3利用者別ACL | 29 | 実Samba/UNFS3、利用者別FUSE接続、POSIX named-user ACL |
| `managed-nas-e2e.json` | 管理下NASの変更と回復 | 68 | 実SMB/NFS、更新/削除各3地点SIGKILL、再送、旧原本保持、別writer拒否 |
| `native-nas-policy.json` | NAS側のwriter制限 | 5 | 実サーバーが直接書込み/指定外clientのサービス資格情報・UIDを拒否 |
| `platform-e2e.json` | Drive→既存RAG→実LLM | 17 | 実Keycloak/session/CSRF、通常API、実NAS、実Anthropic。UI操作/ACL失効/offline/復帰 |
| `core-unit-tests.log` | Storage/RAG単体 | 112 | Storage 54＋RAG 58。実NAS/LLMの根拠とは別 |
| `catalog-tests.log` | カタログ再送/オフラインmountの拒否 | 1 | 実Postgres/OpenFGA。原本blobを作らない |
| `rag-regression-tests.log` | 既存RAG回帰 | 13 | 実DB、一部実FGA。parser/embed等のfakeを含む |
| `chat-replay-tests.log` | 引用・SSE・保存済み回答回帰 | 7 | 実DB＋authz/sourceモック。実NAS/LLM成功件数に混ぜない |

件数はチェックの数であり、独立した要件の網羅率ではない。複数試験で同じ経路を検査するため単純合計を品質指標にしない。
実LLMは既存llm-gateway/ChatWorker経由のAnthropic Haiku 4.5で、質問に含めない検証コード・金額・期限を原本由来で回答した。
同プロファイルの使用量は1,931 tokens、Shiki側の原本blobは0件。抽出本文/埋め込みまでShiki側に保存されないという意味ではない。

実NAS ACLのみを剥奪しShiki FGA権限を残した状態で、原本・依存する保存済み回答本文・引用の遮断を確認した。
自動生成タイトルは残る。開いた画面や既にLLMへ送信したbytesを遡って回収する試験ではない。

## 3. 書込みの成立条件

- 通常共有はread-only。書込みは専用の管理領域で、ネイティブ利用者read-only・指定brokerのみservice writer・均一な読取ポリシーを強制した。
- 顧客が明示したShiki経由の代理書込みであり、利用者の元NAS書込みACLを完全に委任したものではない。
- 操作ID・期待SHA-256、ローカルredb台帳、private stageと旧原本の退避、完成後公開、再送による復旧を使った。
- 退避と公開の間は原本パスが一時不在。原子的なold-or-new CAS、外部アプリの直接編集との競合解決、分散fencing/HAを保証しない。
- 256 KiB/原本、直下名のみ、256操作/領域、旧原本保持、GCなし。削除は物理消去ではない。
- 個別ACL/DACL保存・継承は未対応。そうした既存共有での書込みを安全と判定しない。

## 4. 実測値を使える範囲

初期の独立したGateway/daemonプロファイルの単回値は、1万実ファイルのscanから受領ACKまで59.02秒（169.45件/秒）、
8 MiB全範囲転送2.918秒（2.74 MiB/秒）、観測時daemon RSS 35.77 MiB。
小さなtext2件のRAG初回結果はモデルロード済みで1.93秒。
scanはRAG索引速度ではなく、1 GiB試験はsparse原本の末尾4バイト取得で全転送ではない。
これらは追加したDrive/LLM/管理下NASの負荷値でもrelease性能/SLOでもない。通信総量・百万件・長時間・NAS大量負荷・水平分散は未測定。

## 5. 未対応・未検証・失敗試行

| 領域 | 残る事項 |
| --- | --- |
| OS/共有 | Windows/macOS、kernel CIFS/NFS、物理NAS、Windows DACL、NFSv4 ACL、Kerberos/AD委任、機種差 |
| 通信/identity | 企業proxy/Firewall/WAN、長期offline、正式enrollment/短命PKI/鍵rotation、全失効条件 |
| ファイル/RAG | 安定した移動ID/階層、汎用Office共同編集、全形式/破損文書、連続stream API、8 MiB超の通常Drive取得 |
| 耐障害 | 電源断、ENOSPC、任意命令位置のcrash、媒体故障、全ストア再照合、単独writerの正式fencing |
| 派生データ | タイトル等の引用を持たない生成物、配信済み内容の回収、全履歴の依存関係管理 |
| 配布/性能 | 軽量な端末専用releaseバイナリ、常駐サービス配布/更新、長期GC、百万件/多数顧客のSLO |
| Ops | telemetry外部送信制御、弊社proxy、fleet隔離、署名更新/rollback、Ops停止時の独立稼働は一切検証していない |

初回失敗には、Ganeshaの環境capability/RPC制約、rootless Dockerの接続先、realmのtenant不一致、
アップロード一覧の応答形式誤り、HTTPS proxy CA未設定、Haikuへ未対応adaptive thinkingを送った構成があった。
修正後の成功と区別して元記録`attempts/`へ保持した。CAを明示登録しTLS検証は無効化していない。
長いprovider待機に対しChatWorkerの既定leaseでは再claimが起き得たため、試験構成ではleaseを300秒にした。
lease/heartbeatを製品として解決済みとはみなさない。

当時のRust fmt/clippy・対象回帰・Web codegen/typecheck/lint/buildは成功。リモートCI、全workspace網羅、coverage、
cargo-machete/cargo-denyは未実行。これは今回の文書PRで通すCIとは別の検証範囲である。
**元の全要件達成・本番利用可能とは判定しない。**

## 6. 元記録の照合用SHA-256

元ファイルの内容が変わっていないか確認するためのhash。認証情報・秘密鍵は文書へ転記しない。

| 元記録名 | SHA-256 |
| --- | --- |
| `summary.json` | `00b6e68d09278208d07203aaf17c06411f4a436f846e203724156c3e3db7b736` |
| `e2e.json` | `316b60a6a301dc2834cdc9676ec1ca6e6e32ff9b952ca753bf3b19ba1de5460c` |
| `load-nat.json` | `fc577375e0acb5abc640616a1d4d46e9feaa2b0106c3f6b912a9794a28e920ee` |
| `rag-e2e.json` | `a9b69ed209adcf8e3206ea3e858e6ebdc472dea7948fd20d3801e397ceb6db21` |
| `nas-e2e.json` | `39506881100dde96aa796225a54d12a7e8302b1e53586884da8915d206518f10` |
| `managed-nas-e2e.json` | `d853f3f3ee624bd5b88765dabad8c2fd71362c9e30c856427802a57153a064b7` |
| `native-nas-policy.json` | `6fa05baeec75da469418184a756582fac4ca9398abe20c3cab82d5dc591bb9c0` |
| `platform-e2e.json` | `94fc62c421ed367b657e434f2d436aafe7de9d614fd906730e797682a6827ae9` |
| `poc-unit-tests.log` | `e3c139842af01608ab777363416761428064c9c9677f3da2311b5496f0bf57c4` |
| `chat-replay-tests.log` | `74ee6ae42ce072c86fd17659da6a348f1723db7a6b22b4de6c4f3c8b84281fab` |

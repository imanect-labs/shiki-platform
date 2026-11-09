# Shiki向け floci-gcp インフラ実験

関連: #1139。Phase 8 Task 8.8 / Phase 12 IaCに向けたローカル試作。
本番用Terraformではない。接続先・project・ダミートークンはローカル固定。

## 構成と範囲

- GKEクラスタの管理API（mock）。**k3s・Podは起動しない**。
- 全テナント共有GCSバケット。テナント認可・分離は本実験の対象外。
- 任意のGCS OBJECT_FINALIZE → Pub/Sub通知。Shikiの必須依存ではない。
  Shikiのキューは `crates/jobq`（vanilla Postgres、pgmq不使用）。
- floci-gcpイメージは検証したdigest、Google providerはlockfileで固定。

Shikiの `storage.backend=gcs` は現在 `crates/api/src/wiring.rs` で未実装エラーになる。
本実験はShikiの起動・アップロード・索引作成を検証しない。
Shiki全体にはPostgres、OpenFGA、Keycloak、Redis、Qdrant等も必要であり、
GKE/GCSだけでアプリ環境が完成するわけではない。

## 実行

必要: Docker Compose、Terraform 1.14以上（2未満）、Python 3。
4588番ポートが空いていること。GKE用ホスト `container.localhost` がループバックへ
解決されること（`getent hosts container.localhost` 等で確認）。
TerraformのGKEクライアントはパス接頭辞を除去するため、GKEだけこのホスト名を使う。

```bash
cd deploy/floci-gcp
docker compose up -d
curl --noproxy '*' --fail http://127.0.0.1:4588/health
# /health が成功してから実行
export NO_PROXY="${NO_PROXY:+$NO_PROXY,}127.0.0.1,localhost,container.localhost"
terraform init
terraform fmt -check
terraform validate
terraform apply
python3 smoke.py
terraform plan -detailed-exitcode  # 差分なしなら終了コード0
```

通知も試す場合は、次の一貫した変数指定を使う。
（Pub/Subは既定で無効。trueでapplyした後に指定を省略すると削除planになる。）

```bash
terraform apply -var=enable_pubsub_experiment=true
python3 smoke.py --pubsub
terraform plan -detailed-exitcode -var=enable_pubsub_experiment=true
```

smokeは一意なオブジェクトをアップロード・取得し、終了時に削除する。
`--pubsub` では該当オブジェクトの通知本文・bucket・eventTypeを検証しackする。
IAM・Workload Identity・署名URL・ネットワークポリシーは検証しない。
実GCPではGCSサービスエージェントへのtopic publish権限などが別途必要。

## 後片付け

自分のローカル実験リソースのみを消す。バケット内オブジェクトも削除される。

```bash
terraform destroy -var=enable_pubsub_experiment=true
docker compose down
```

エミュレーターはmemoryモード。Terraform stateだけが永続化されるので、
コンテナを作り直した場合は再applyしてリソースを再作成する。
state/planはgit管理対象外、provider lockfileは管理対象。

## 実k3sでの検証（Docker socket提供を明示承認した場合）

`compose.k3s.yaml` はホストDockerソケットを外部イメージへ渡す。
floci-gcpはその権限でprivilegedなk3sサイドカーを作成する。
既存のmock構成は先に `terraform destroy`、`docker compose down` で片付ける。

```bash
docker compose -f compose.yaml -f compose.k3s.yaml up -d
# /health 成功後、上記のNO_PROXYを設定して実行
terraform apply -var=enable_pubsub_experiment=true
python3 smoke.py --pubsub
bash smoke-k3s.sh
```

`smoke-k3s.sh` はこの実験のサイドカーだけに接続し、実ノードのReadyと
一意なKubernetes Jobの完了・ログを検証し、Jobを削除する。
Terraform apply成功やGKEのRUNNING表示だけではPod実行成功と見なさない。
終了時は `terraform destroy -var=enable_pubsub_experiment=true` の後、
`docker compose -f compose.yaml -f compose.k3s.yaml down` を実行する。

### このクラウド実行環境の制約

ユーザー承認後、実k3sでTerraform applyは成功し、endpointは `localhost:6550` となった。
ただしkubelet起動に失敗し、ノードReady・Pod実行は検証できなかった。

1. 既定overlayfs snapshotterが `invalid argument` で失敗。
2. サイドカーだけ `snapshotter: native` にすると先へ進むが `/dev/kmsg` が存在しない。
3. ログ推奨の `KubeletInUserNamespace=true` を加えると、最終的に
   `mkdir /sys/fs/cgroup/kubepods: permission denied` で停止。

これらは調査用にサイドカーへ直接適用した変更で、Composeには含めていない。
ホストのcgroup権限を変更する回避策は試していない。
実Pod検証には、ネストしたk3sのcgroup管理を許すDockerホストが必要。

## 検証記録（2026-11-09）

- Terraform 1.14.0 / hashicorp/google 7.46.2 / floci-gcp 0.12.0。
- `terraform validate`: Success。
- Pub/Sub有効時: `Apply complete! Resources: 5 added, 0 changed, 0 destroyed.`
- `python3 smoke.py --pubsub`: GKE管理API、GCS読み書き、GCS→Pub/Sub pull/ack 全てPASS。
- 再plan: `No changes. Your infrastructure matches the configuration.`
- 実k3sは承認後に起動を試行。Terraform作成成功、kubeletは上記cgroup権限で停止。
  ノードReady・Pod実行・Shikiアプリ接続は未検証。

既存設計書は変更していない。`docs/roadmap.md` はクラウド実装を各フェーズへ分散した
方針だが、GCS実装や一部コメントには旧Phase 8の記述が残っているため、
アプリ接続を進める際に整合の確認が必要。

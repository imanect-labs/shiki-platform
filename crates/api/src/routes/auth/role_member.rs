//! ロール・部署の member の直接タプル（`role:<tenant>|<id>#member@user:<tenant>|<sub>`）を読む・外す手順と、
//! member であるべきロールの決め方（[`desired_roles`]）。
//!
//! ログインとセッションの更新のロールの同期（[`super::callback::provision_roles`]）と、Keycloak の所属グループとの
//! 定期の照合（[`crate::membership_reconcile`]・D-136・D-140）が同じ手順と同じ決め方を使う（写しを作らない）。
//! どれを外すかの候補は呼ぶ側が決める（ログインの同期は今の直接タプルのすべて、照合はログインの同期が記録したもの）。
//!
//! 付け外しは、システムを主体にした監査に、1 回の同期の付与・剥奪をロールを持つ組織（[`role_owner_org`]）ごとに 1 行ずつ
//! 残す（[`super::membership_audit`]・GAP-78）。付けたことを記録できなければ付けたタプルを外し、外したことを記録できなく
//! ても外したものは戻さない。付ける書き込みが誤りで返ったとき（応答が切れても適用されていることがある）は、今の直接
//! タプルを読み直し、あれば付けたものとして記録し、読めなければ外して、外したことを記録する（記録の無い付与を残さない）。
//! 同期の初めに今の直接タプルを読めなければ、付けも外しもしない（[`sync_roles`]）。

use authz::{AuthContext, AuthzClient, GroupLayout, Namespace, ObjectType, Relation, Subject};

use super::membership_audit::MembershipAudit;
use crate::extract::org_of_groups;

/// member であるべきロール（[`desired_roles`] の結果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesiredRoles {
    /// 所属グループから決まる組織（決まらなければ既定の組織・`AuthContext` と同じ規則）。
    pub org: String,
    /// ロールの id と、所属グループ由来（部署）か。同じ id が属性 `roles` と所属グループの両方にあれば属性の方。
    pub roles: Vec<(String, bool)>,
}

impl DesiredRoles {
    /// このロールの member であるべきか。
    pub(crate) fn contains(&self, role: &str) -> bool {
        self.roles.iter().any(|(id, _)| id == role)
    }
}

/// 属性 `roles`（ロールの claim の元）と所属グループ（`groups`）から、member であるべきロールを決める
/// （ログインの同期と定期の照合の単一定義）。
///
/// - 部署は自テナント配下の所属グループだけ（`/tenants/<tenant>/<org>/<dept>` → `<org>/<dept>`・#811）。他テナント
///   配下・（multi では）テナント無修飾のグループはロールにしない。**組織そのもののグループ（`<org>`）は除く**
///   （組織は organization のタプルの責務）。属性 `roles` に組織と同じ名前のロールがあっても、それは正当なロール
///   なので除かない（除くと同期が誤って外す）。
/// - 識別子を壊す値（空・FGA の構造文字・制御文字）は [`authz::validate_local_id`]（文字の決まりの単一定義・
///   #91 M-3）で除く。
pub(crate) fn desired_roles(
    roles: &[String],
    groups: &[String],
    tenant_id: &str,
    layout: GroupLayout,
) -> DesiredRoles {
    let org = org_of_groups(groups, tenant_id, layout);
    let departments = authz::org_group::tenant_relative_groups(groups, tenant_id, layout);
    let roles = role_sources(roles, &departments, &org);
    DesiredRoles { org, roles }
}

/// [`desired_roles`] の本体（部署はテナントからの相対パスにしたもの）。
pub(crate) fn role_sources(
    roles: &[String],
    departments: &[String],
    org: &str,
) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let candidates = roles
        .iter()
        .map(|r| (r.trim().to_string(), false))
        .chain(departments.iter().map(|d| (d.clone(), true)));
    for (id, from_group) in candidates {
        let id = id.trim();
        if (from_group && id == org) || authz::validate_local_id(id).is_err() {
            continue;
        }
        if !out.iter().any(|(existing, _)| existing == id) {
            out.push((id.to_string(), from_group));
        }
    }
    out
}

/// 利用者が直接タプルを持つこのテナントのロール（継承は展開しない・他テナントのタプルは除く）。
///
/// 読めないときは `None`（呼ぶ側は外さない。未知の状態に破壊的な操作をしない）。
pub(crate) async fn current_roles(
    authz: &dyn AuthzClient,
    subject: &Subject,
    ns: &Namespace<'_>,
) -> Option<Vec<String>> {
    match authz.read_subject_objects(subject, ObjectType::Role).await {
        Ok(objects) => Some(
            objects
                .iter()
                .filter_map(|raw| raw.strip_prefix("role:"))
                .filter_map(|id_part| ns.strip_object_id(id_part))
                .map(str::to_string)
                .collect(),
        ),
        Err(e) => {
            tracing::warn!(error = %e, "ロールの直接タプルを読めません");
            None
        }
    }
}

/// ロールを持つ組織（監査の行の org の列と、台帳 `directory_role` の org の列）。
///
/// 所属グループから来たロール（`from_group`）の id はテナントからの相対パス（部署の `<org>/<dept>` と、2 つ目の組織の
/// グループの `<org>`・[`desired_roles`]）なので、先頭のセグメントの組織が持ち主になる（識別子にできる名前のときだけ）。
/// 属性 `roles` のロールは、`/` を含んでも接頭辞では決めず、`user_org`（利用者の組織・`AuthContext` と同じ規則）。属性の
/// 値の接頭辞は組織とは限らない（`sales/manager`）ので、接頭辞で決めると、存在しない組織やほかの組織の監査に書いてしまう。
/// 付けるときの持ち主を台帳から引かないのは、属性 `roles` のロールの台帳の組織が、最後に付けた人の組織で書き換わるため
/// （外すロールがどこから来たかは、台帳から決める・[`with_ledger_sources`]）。
pub(crate) fn role_owner_org<'a>(role: &'a str, from_group: bool, user_org: &'a str) -> &'a str {
    match role.split('/').next() {
        Some(org) if from_group && authz::validate_local_id(org).is_ok() => org,
        _ => user_org,
    }
}

/// `roles`（ロールの id と、所属グループから来たか）を、ロールを持つ組織（[`role_owner_org`]）ごとに束ねる（組織は
/// 初めて出た順・組織の中はもとの順）。
pub(crate) fn roles_by_owner(
    roles: &[(String, bool)],
    user_org: &str,
) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (role, from_group) in roles {
        let owner = role_owner_org(role, *from_group, user_org);
        match out.iter_mut().find(|(org, _)| org == owner) {
            Some((_, group)) => group.push(role.clone()),
            None => out.push((owner.to_string(), vec![role.clone()])),
        }
    }
    out
}

/// 外す候補（今の直接タプルから決めたもので、どこから来たかは分からない）に、所属グループから来たロールかを添える。
///
/// 台帳（`directory_role`）の組織がロールの先頭のセグメントと同じなら、所属グループから来たロールとする（付けたときに
/// [`role_owner_org`] でその組織のロールとして台帳に書いたもの）。台帳に行が無い・組織が違う・読めないロールは、属性
/// `roles` のロールとして扱う（外した行は利用者の組織に書く）。接頭辞だけで決めないのは、属性 `roles` の `acme/admin`
/// （ほかの組織の名前の接頭辞）や `sales/manager`（組織でない接頭辞）を、その組織のロールとして書かないため。
///
/// 限界: 今の直接タプルからはどこから来たかが分からないので、属性 `roles` のロールでも、台帳がその接頭辞の組織のロール
/// としていれば（その組織の部署と同じ id か、その組織の人が最後に付けた）、外した行はその組織に書く。
pub(crate) async fn with_ledger_sources(
    directory: &storage::DirectoryStore,
    tenant_id: &str,
    roles: &[String],
) -> Vec<(String, bool)> {
    let ledger = match directory.role_orgs(tenant_id, roles).await {
        Ok(ledger) => ledger,
        Err(e) => {
            tracing::warn!(error = %e, "directory_role を読めません（外したロールの行は利用者の組織に書く）");
            Vec::new()
        }
    };
    roles
        .iter()
        .map(|role| {
            let head = role.split('/').next().unwrap_or_default();
            let from_group = ledger.iter().any(|(id, org)| id == role && org == head);
            (role.clone(), from_group)
        })
        .collect()
}

/// `stale`（ロールの id と、所属グループから来たか）の member を外し、実際に外したロールを返す（無いものは no-op・失敗は
/// 次の同期に任せる）。
///
/// 外したロールは、外してからロールを持つ組織（[`role_owner_org`]）ごとに 1 行にまとめて監査に残す（記録できなくても
/// 戻さない・[`MembershipAudit::roles_revoked`]）。
pub(crate) async fn revoke_roles(
    authz: &dyn AuthzClient,
    audit: &MembershipAudit<'_>,
    user_org: &str,
    subject: &Subject,
    ns: &Namespace<'_>,
    stale: &[(String, bool)],
) -> Vec<String> {
    let mut revoked = Vec::new();
    for (role, from_group) in stale {
        match authz
            .delete_tuple(subject, Relation::Member, &ns.role(role))
            .await
        {
            Ok(true) => {
                tracing::info!(%role, "ロールの member を外した（所属から消えた）");
                revoked.push((role.clone(), *from_group));
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(%role, error = %e, "ロールの member を外せません（次の同期で外し直す）");
            }
        }
    }
    for (owner, roles) in roles_by_owner(&revoked, user_org) {
        audit.roles_revoked(&owner, &roles).await;
    }
    revoked.into_iter().map(|(role, _)| role).collect()
}

/// ロールの直接タプルと台帳（`directory_role`）を、あるべき集合（`desired`・claims 由来のロールの id と、所属グループから
/// 来たか・[`desired_roles`]）にそろえる（ログインとセッションの更新の同期の本体・[`super::callback::provision_roles`]）。
/// タプルを変えたかを返す。
///
/// 現在の直接タプル（自テナント名前空間のみ・継承展開なし）が読めない時は 1 回だけ読み直し、それでも読めなければ
/// **剥奪も付与もしない**（次の同期に回す）。剥奪をしないのは誤剥奪を避けるため（unknown state に破壊的な操作をしない）。
/// 付与をしないのは、もとの集合が分からないまま書くと、書き込みが誤りで返ったときに、もとからあったロールと付けた
/// ロールを見分けられないため（もとからあったロールを付けたと記録するか、付けたロールを記録しないまま残す）。どちらも
/// 権限が減る向きで、次の同期で付けて記録する。
///
/// 剥奪は 現在 − あるべき。**claims を正本とする**（callback.rs 冒頭の設計）ため、claims が空＝ロール無しと解釈し
/// stale タプルを剥奪する。IdP でユーザーの最後の role/group が外れたケース（desired 空）で過剰権限を残さないため
/// （剥奪漏れ＝fail-open を避ける・#91 P1 レビュー対応）。過剰剥奪は fail-safe（アクセス減・次回の正しいログインで
/// 再付与）で自己回復する。一過性の IdP 不調そのものは、現在集合の READ が失敗した時のみ何もしない上のガードで扱い、
/// READ 成功時の空 claims は正当な「ロール無し」として扱う。
///
/// 付け外しは `audit` で監査に残す（org の列はロールを持つ組織・[`role_owner_org`]。外すロールがどこから来たかは
/// 台帳で決める・[`with_ledger_sources`]）。
pub(crate) async fn sync_roles(
    authz: &dyn AuthzClient,
    directory: &storage::DirectoryStore,
    audit: &MembershipAudit<'_>,
    ctx: &AuthContext,
    desired: &[(String, bool)],
) -> bool {
    let (subject, ns) = (ctx.subject(), ctx.ns());
    let mut current = current_roles(authz, &subject, &ns).await;
    if current.is_none() {
        current = current_roles(authz, &subject, &ns).await;
    }
    let Some(current) = current else {
        tracing::warn!(
            "ロールの直接タプルを読み直しても読めないので、この同期では付けも外しもしない"
        );
        return false;
    };
    let stale: Vec<String> = current
        .iter()
        .filter(|c| !desired.iter().any(|(id, _)| id == *c))
        .cloned()
        .collect();
    let revoked = if stale.is_empty() {
        false
    } else {
        let stale = with_ledger_sources(directory, ctx.tenant_id(), &stale).await;
        !revoke_roles(authz, audit, ctx.org(), &subject, &ns, &stale)
            .await
            .is_empty()
    };
    let granted = grant_roles(authz, directory, audit, ctx, desired, &current).await;
    revoked || granted
}

/// あるべきロールの member を付け、台帳（`directory_role`）をそろえる。タプルを足したかを返す（付与は あるべき − 現在。
/// 冪等 write なので差分に限らず流しても安全だが、無駄打ちを避ける）。
///
/// - 台帳の行の組織は、ロールを持つ組織（[`role_owner_org`]・部署はその部署の組織、属性 `roles` のロールは利用者の
///   組織）。付けた人の組織で書くと、ほかの組織の部署がその組織のロールの検索と `role_exists` に出る。
/// - 今タプルがある（`current` にある）ロールも、台帳の行が欠けていれば戻す（BUG-270）。台帳は共有の相手検索・ロール
///   の列の参照の照合・テナント撤去の列挙（`purge_tenant`）が読むので、タプルだけが残ると、ロールを書けず撤去からも
///   漏れる。行があれば表示名・作成日時を変えない。部署の行の組織が持ち主と違えば（付けた人の組織で書いていた前の行）
///   直す（[`restore_ledger`]）。
/// - 今タプルが無いロールは、台帳を先に書いてからタプルを足す（#91 M-4）。タプルを先に書くと、台帳の書き込みに
///   失敗したとき「タプルはあるが台帳に無いロール」ができ、撤去の列挙から漏れる。台帳を書けなければタプルを
///   足さない（次の同期で試し直す）。
/// - 実際に書いたロール（`write_tuple` が `true`）だけを、ロールを持つ組織ごとに 1 行にまとめて監査に残す。記録できな
///   ければ、その組織のぶんの書いたタプルを外す（記録の無い付与を残さない・次の同期で付け直して記録する）。台帳の行は
///   残す（タプルを外すときと同じ）。
/// - `write_tuple` が誤りで返ったロールは、適用されたかが分からない（応答が切れても書けていることがある）。
///   [`settle_unknown_writes`] で確かめ、あれば書いたロールとして記録する（次の同期では `current` に入って記録されない
///   ので、ここで記録しないと記録の無い付与が残り続ける）。
async fn grant_roles(
    authz: &dyn AuthzClient,
    directory: &storage::DirectoryStore,
    audit: &MembershipAudit<'_>,
    ctx: &AuthContext,
    desired: &[(String, bool)],
    current: &[String],
) -> bool {
    let (tenant_id, user_org) = (ctx.tenant_id(), ctx.org());
    let (held, missing): (Vec<_>, Vec<_>) = desired
        .iter()
        .cloned()
        .partition(|(id, _)| current.contains(id));
    restore_ledger(directory, tenant_id, user_org, &held).await;
    let (subject, ns) = (ctx.subject(), ctx.ns());
    let mut granted = false;
    let mut written: Vec<(String, bool)> = Vec::new();
    let mut unknown: Vec<(String, bool)> = Vec::new();
    for (role_id, from_group) in missing {
        let owner = role_owner_org(&role_id, from_group, user_org);
        if let Err(e) = directory
            .upsert_role(&role_id, tenant_id, owner, &role_id)
            .await
        {
            tracing::warn!(role = %role_id, error = %e, "directory_role の upsert に失敗（タプル付与を保留・次回同期で再試行）");
            continue;
        }
        match authz
            .write_tuple(&subject, Relation::Member, &ns.role(&role_id))
            .await
        {
            Ok(wrote) => {
                granted = true;
                if wrote {
                    written.push((role_id, from_group));
                }
            }
            Err(e) => {
                tracing::warn!(role = %role_id, error = %e, "role 付与が誤りで返った（適用されたかを確かめる・login は継続）");
                unknown.push((role_id, from_group));
            }
        }
    }
    let applied = settle_unknown_writes(authz, audit, user_org, &subject, &ns, &unknown).await;
    granted |= !applied.is_empty();
    written.extend(applied);
    record_granted(authz, audit, user_org, &subject, &ns, &written).await;
    granted
}

/// タプルが既にあるロール（`held`）の台帳の欠けた行を戻す（BUG-270）。部署（所属グループから来たロール）は持ち主の
/// 組織ごとに書き、組織が違う行も直す（`ensure_group_roles`）。属性 `roles` のロールは利用者の組織で、行があれば
/// 何も変えない（`ensure_roles`・台帳の組織は最後に付けた人の組織のまま）。失敗しても次の同期で試し直す。
async fn restore_ledger(
    directory: &storage::DirectoryStore,
    tenant_id: &str,
    user_org: &str,
    held: &[(String, bool)],
) {
    let (groups, attrs): (Vec<_>, Vec<_>) = held
        .iter()
        .cloned()
        .partition(|(_, from_group)| *from_group);
    for (owner, roles) in roles_by_owner(&groups, user_org) {
        let done = directory
            .ensure_group_roles(tenant_id, &owner, &roles)
            .await;
        log_restored(&owner, done);
    }
    let attrs: Vec<String> = attrs.into_iter().map(|(role, _)| role).collect();
    log_restored(
        user_org,
        directory.ensure_roles(tenant_id, user_org, &attrs).await,
    );
}

/// [`restore_ledger`] の 1 回の書き込みの結果をログに残す。
fn log_restored(org: &str, done: Result<u64, storage::StorageError>) {
    match done {
        Ok(0) => {}
        Ok(restored) => {
            tracing::info!(
                restored, %org,
                "directory_role の欠けていた行を戻すか、部署の行の組織を直した（タプルはあった）"
            );
        }
        Err(e) => {
            tracing::warn!(%org, error = %e, "directory_role の欠けを戻せません（次回同期で再試行）");
        }
    }
}

/// 書き込みが誤りで返ったロール（`unknown`）が実際に書けているかを、今の直接タプルを読み直して確かめ、書けていたロールを
/// 返す（`tuple_commit.rs` の決まり (a) と同じく、誤りでも適用されていることがあるものとして扱う）。
///
/// 読み直せなければ、書けていたかもしれないタプルを外す（冪等・記録の無い付与を残さない・権限が減る向きで、次の同期で
/// 付け直して記録する）。`unknown` は同期の初めに読んだ今の集合に無かったロールなので、外すのはこの書き込みが足したものか、
/// 同じ人の並行する同期（ログインとセッションの更新は直列にしていない）が足したもの。実際に外したものは、外したと記録
/// する（並行する同期が付けて記録した member を外したときも、監査とタプルが食い違わない）。並行する同期が付けた member を
/// 読み直しで見つけると、同じ付与を 2 回記録することがある（まれ・権限は変わらない）。
///
/// 読み直しは誤りを受けた直後なので、クライアントが打ち切った後でサーバーが書き込みを確定させたときは、無いと答えて
/// 外しもしない。そのときは記録の無い付与が残る（狭い時間の窓・定期の照合では見つけない）。
async fn settle_unknown_writes(
    authz: &dyn AuthzClient,
    audit: &MembershipAudit<'_>,
    user_org: &str,
    subject: &Subject,
    ns: &Namespace<'_>,
    unknown: &[(String, bool)],
) -> Vec<(String, bool)> {
    if unknown.is_empty() {
        return Vec::new();
    }
    if let Some(held) = current_roles(authz, subject, ns).await {
        return unknown
            .iter()
            .filter(|(role, _)| held.contains(role))
            .cloned()
            .collect();
    }
    let mut removed = Vec::new();
    for (role, from_group) in unknown {
        match authz
            .delete_tuple(subject, Relation::Member, &ns.role(role))
            .await
        {
            Ok(true) => removed.push((role.clone(), *from_group)),
            Ok(false) => {}
            Err(e) => {
                tracing::error!(%role, error = %e,
                    "誤りで返った role 付与を確かめられず、外すこともできません（記録の無い付与が残っているかもしれない）");
            }
        }
    }
    for (owner, roles) in roles_by_owner(&removed, user_org) {
        audit.roles_revoked(&owner, &roles).await;
    }
    Vec::new()
}

/// 書いたロール（`written`）を、ロールを持つ組織（[`role_owner_org`]）ごとに 1 行にまとめて監査に残す。記録できなかった
/// 組織のぶんは、書いたタプルを外す（記録の無い付与を残さない・次の同期で付け直して記録する）。
async fn record_granted(
    authz: &dyn AuthzClient,
    audit: &MembershipAudit<'_>,
    user_org: &str,
    subject: &Subject,
    ns: &Namespace<'_>,
    written: &[(String, bool)],
) {
    for (owner, roles) in roles_by_owner(written, user_org) {
        let Err(e) = audit.roles_granted(&owner, &roles).await else {
            continue;
        };
        tracing::error!(org = %owner, ?roles, error = %e, "ロールの member を付けたことを監査に残せません（付けたタプルを外す）");
        for role in &roles {
            if let Err(e) = authz
                .delete_tuple(subject, Relation::Member, &ns.role(role))
                .await
            {
                tracing::error!(%role, error = %e,
                    "監査に残せなかったロールの member を外せません（記録の無い付与が残っている・手で確かめる）");
            }
        }
    }
}

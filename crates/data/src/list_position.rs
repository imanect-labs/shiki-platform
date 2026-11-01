//! ほかのサービスの一覧の続きの位置を、テナントに束縛して暗号化する（BUG-162）。
//!
//! ドライブの「共有されたもの」とアーティファクトの一覧の共有分は、ListObjects の応答が上限に届くと、DB の候補を
//! 並びの順に引いて OpenFGA で絞る（[`authz::visible`]）。1 回の頼みで問う数に上限があるので、続きの位置は見えない
//! ものを指すことがある。位置をそのまま出すと見えないものの id と更新の時刻が分かるので、テーブルの一覧の続きの
//! 位置（[`crate::table_list`]）と同じ鍵で暗号化する。鍵を持つのが data なので、API がここを通して封をする。
//!
//! ドライブのフォルダーの中身の一覧・名前の検索と、ワークフローの「フォルダ一覧」の続きの位置（BUG-187・D-131）は、
//! StorageService が作る不透明な文字列（並びの値と id）を、テナント・一覧の種類・一覧の範囲（フォルダー・検索の語・
//! 並び）に束縛して暗号化する（[`DataStore::seal_list_cursor`]）。並びの値は見えないノードの名前のことがある。

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::cursor::CursorCodec;
use crate::store::DataStore;
use crate::{DataError, InvalidCode};

/// 一覧の種類（AAD に入れる・別の一覧の位置を持ち込ませない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListPositionKind {
    /// ドライブの「共有されたもの」（`GET /shares/shared-with-me`）。
    DriveShared,
    /// アーティファクトの一覧の共有分（`GET /artifacts` の `shared_cursor`）。
    ArtifactsShared,
}

impl ListPositionKind {
    fn label(self) -> &'static [u8] {
        match self {
            ListPositionKind::DriveShared => b"drive-shared",
            ListPositionKind::ArtifactsShared => b"artifacts-shared",
        }
    }
}

/// 中身を持たない続きの位置（ほかのサービスが作った不透明な文字列）の一覧の種類（AAD に入れる・BUG-187・D-131）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListCursorKind {
    /// ドライブのフォルダーの中身（`GET /nodes`・アプリのゲートウェイのフォルダーの一覧）。
    DriveFolder,
    /// ドライブの名前の検索（`GET /nodes?q=`）。
    DriveSearch,
    /// ワークフローの「フォルダ一覧」（storage.list・`Shiki.storage.list`・AI エージェントの道具）。
    WorkflowFolder,
}

impl ListCursorKind {
    fn label(self) -> &'static [u8] {
        match self {
            ListCursorKind::DriveFolder => b"drive-folder",
            ListCursorKind::DriveSearch => b"drive-search",
            ListCursorKind::WorkflowFolder => b"workflow-folder",
        }
    }

    /// 種類と範囲を 1 つの束縛にする（AAD の一覧の種類の欄・長さで区切られる）。ラベルは NUL を含まないので、
    /// 種類と範囲の境目は 1 通りに決まる。
    fn binding(self, scope: &str) -> Vec<u8> {
        let mut b = self.label().to_vec();
        b.push(0);
        b.extend_from_slice(scope.as_bytes());
        b
    }
}

/// 一覧の続きの位置（並びのキー `(updated_at, id)` の最後の値・キーセット）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListPosition {
    pub updated_at: DateTime<Utc>,
    pub id: Uuid,
}

/// 受け付ける位置の長さの上限（復号の前に弾く）。
const MAX_POSITION_LEN: usize = 256;

impl DataStore {
    /// 一覧の続きの位置を暗号化する（不透明な文字列）。
    pub fn seal_list_position(
        &self,
        kind: ListPositionKind,
        tenant_id: &str,
        pos: ListPosition,
    ) -> Result<String, DataError> {
        seal(&self.cursor, kind, tenant_id, pos)
    }

    /// [`Self::seal_list_position`] の逆。読めない位置（改竄・別テナント・別の一覧・前の版の平文の位置を含む）は
    /// `cursor_invalid`（400・先頭から読み直す）。
    pub fn open_list_position(
        &self,
        kind: ListPositionKind,
        tenant_id: &str,
        token: &str,
    ) -> Result<ListPosition, DataError> {
        open(&self.cursor, kind, tenant_id, token)
    }
}

impl DataStore {
    /// ほかのサービスが作った続きの位置（不透明な文字列）を、テナント・一覧の種類・一覧の範囲（`scope`。フォルダーの
    /// id・検索の語・並びなど、同じ位置を使ってよい一覧を 1 つに決める文字列）に束縛して暗号化する（BUG-187・D-131）。
    pub fn seal_list_cursor(
        &self,
        kind: ListCursorKind,
        tenant_id: &str,
        scope: &str,
        raw: &str,
    ) -> Result<String, DataError> {
        self.cursor
            .seal_list_position(&kind.binding(scope), tenant_id, raw.as_bytes())
    }

    /// [`Self::seal_list_cursor`] の逆。読めない位置（改竄・別テナント・別の一覧・別の範囲・前の版の平文の位置を
    /// 含む）は `cursor_invalid`（400・先頭から読み直す）。
    pub fn open_list_cursor(
        &self,
        kind: ListCursorKind,
        tenant_id: &str,
        scope: &str,
        token: &str,
    ) -> Result<String, DataError> {
        open_cursor(&self.cursor, kind, tenant_id, scope, token)
    }
}

fn open_cursor(
    codec: &CursorCodec,
    kind: ListCursorKind,
    tenant_id: &str,
    scope: &str,
    token: &str,
) -> Result<String, DataError> {
    let raw = codec.open_list_position(&kind.binding(scope), tenant_id, token)?;
    String::from_utf8(raw).map_err(|_| DataError::input(InvalidCode::CursorInvalid))
}

fn seal(
    codec: &CursorCodec,
    kind: ListPositionKind,
    tenant_id: &str,
    pos: ListPosition,
) -> Result<String, DataError> {
    let raw = format!("{}.{}", pos.updated_at.timestamp_micros(), pos.id);
    codec.seal_list_position(kind.label(), tenant_id, raw.as_bytes())
}

fn open(
    codec: &CursorCodec,
    kind: ListPositionKind,
    tenant_id: &str,
    token: &str,
) -> Result<ListPosition, DataError> {
    let bad = || DataError::input(InvalidCode::CursorInvalid);
    if token.len() > MAX_POSITION_LEN {
        return Err(bad());
    }
    let raw = codec.open_list_position(kind.label(), tenant_id, token)?;
    let raw = String::from_utf8(raw).map_err(|_| bad())?;
    let (micros, id) = raw.split_once('.').ok_or_else(bad)?;
    let updated_at = micros
        .parse::<i64>()
        .ok()
        .and_then(DateTime::from_timestamp_micros)
        .ok_or_else(bad)?;
    let id = Uuid::parse_str(id).map_err(|_| bad())?;
    Ok(ListPosition { updated_at, id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::CursorKey;

    fn codec(k: u8) -> CursorCodec {
        CursorCodec::new(&CursorKey::from_bytes([k; crate::CURSOR_KEY_LEN]))
    }

    #[test]
    fn positions_round_trip_and_stay_bound() {
        let c = codec(7);
        let pos = ListPosition {
            updated_at: DateTime::from_timestamp_micros(1_700_000_000_123_456).unwrap(),
            id: Uuid::new_v4(),
        };
        let drive = ListPositionKind::DriveShared;
        let token = seal(&c, drive, "t-a", pos).unwrap();
        assert!(!token.contains(&pos.id.to_string()), "中身が読めない");
        assert_eq!(open(&c, drive, "t-a", &token).unwrap(), pos);
        let invalid = |r: Result<ListPosition, DataError>| matches!(&r, Err(DataError::InvalidInput(i)) if i.code() == InvalidCode::CursorInvalid);
        // 別テナント・別の一覧・別の鍵は読めない。
        assert!(invalid(open(&c, drive, "t-b", &token)));
        assert!(invalid(open(
            &c,
            ListPositionKind::ArtifactsShared,
            "t-a",
            &token
        )));
        assert!(invalid(open(&codec(8), drive, "t-a", &token)));
        // 改竄・空・前の版の平文の位置（hex）・長すぎる位置は読めない。
        let mut tampered = token.clone().into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).unwrap();
        let long = "x".repeat(300);
        for bad in [tampered.as_str(), "", "abc", "3161626364", long.as_str()] {
            assert!(invalid(open(&c, drive, "t-a", bad)), "{bad:?}");
        }
    }

    /// 不透明な続きの位置は、テナント・一覧の種類・範囲のどれが違っても読めない（BUG-187・D-131）。
    #[test]
    fn opaque_cursors_stay_bound_to_tenant_kind_and_scope() {
        let c = codec(7);
        let folder = ListCursorKind::DriveFolder;
        // 中身は見えないノードの名前を含みうる（並びの値）。
        let raw = "6e0f1c2d3e-非公開の人事評価.xlsx";
        let token = c
            .seal_list_position(&folder.binding("n:asc:f-1"), "t-a", raw.as_bytes())
            .unwrap();
        assert!(!token.contains("人事"), "中身が読めない");
        assert_eq!(
            open_cursor(&c, folder, "t-a", "n:asc:f-1", &token).unwrap(),
            raw
        );
        let invalid = |r: Result<String, DataError>| matches!(&r, Err(DataError::InvalidInput(i)) if i.code() == InvalidCode::CursorInvalid);
        assert!(
            invalid(open_cursor(&c, folder, "t-b", "n:asc:f-1", &token)),
            "別テナント"
        );
        assert!(
            invalid(open_cursor(&c, folder, "t-a", "n:asc:f-2", &token)),
            "別のフォルダー"
        );
        assert!(
            invalid(open_cursor(&c, folder, "t-a", "n:desc:f-1", &token)),
            "別の並び"
        );
        for other in [ListCursorKind::DriveSearch, ListCursorKind::WorkflowFolder] {
            assert!(
                invalid(open_cursor(&c, other, "t-a", "n:asc:f-1", &token)),
                "{other:?}"
            );
        }
        // 範囲と種類の境目をずらしても同じ束縛にならない。
        assert_ne!(
            folder.binding("x"),
            ListCursorKind::DriveSearch.binding("x")
        );
        assert!(
            invalid(open_cursor(&codec(8), folder, "t-a", "n:asc:f-1", &token)),
            "別の鍵"
        );
        // 共有されたもの（中身のある位置）の暗号文は読めない。
        let shared = seal(
            &c,
            ListPositionKind::DriveShared,
            "t-a",
            ListPosition {
                updated_at: DateTime::from_timestamp_micros(1).unwrap(),
                id: Uuid::nil(),
            },
        )
        .unwrap();
        assert!(invalid(open_cursor(&c, folder, "t-a", "", &shared)));
        assert!(
            invalid(open_cursor(&c, folder, "t-a", "n:asc:f-1", "6e3a")),
            "前の版の平文の位置"
        );
    }
}

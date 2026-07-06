//! Firecracker ゲストエージェントとホストの vsock プロトコル（フレーム＝u32-LE 長さ前置＋JSON）。
//!
//! 1 接続・厳密逐次: ホストが [`Request`] を 1 つ送り、ゲストが 0 個以上のストリーミング [`Event`]
//! （`Stdout`/`Stderr`）に続いて終端イベント（`Exited`/`File`/`Dir`/`Ok`/`Err`）を返す。多重化しない。

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// フレーム本文の上限（敵対的入力から双方を守る・32 MiB）。
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// ホスト→ゲストの要求。バイナリは base64 で運ぶ（JSON 安全）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// プロセスを起動する（argv 直接・シェル解釈なし）。timeout_ms 秒で pgroup ごと kill。
    Exec { argv: Vec<String>, timeout_ms: u64 },
    /// ファイルを書く（`b64` は内容の base64）。
    WriteFile { path: String, b64: String },
    /// ファイルを読む。
    ReadFile { path: String },
    /// ディレクトリを一覧する。
    ListDir { path: String },
    /// ゲストを停止する（VM を電源オフ）。
    Shutdown,
}

/// ゲスト→ホストのイベント。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event {
    /// 起動完了通知（接続直後に 1 度）。
    Ready { version: String },
    /// 標準出力チャンク（base64）。
    Stdout { b64: String },
    /// 標準エラーチャンク（base64）。
    Stderr { b64: String },
    /// プロセス終了。
    Exited { code: i32 },
    /// ReadFile 応答（base64）。
    File { b64: String },
    /// ListDir 応答。
    Dir { entries: Vec<DirEntryDto> },
    /// 副作用系（WriteFile/Shutdown）の成功応答。
    Ok,
    /// エラー応答。
    Err { msg: String },
}

/// ディレクトリエントリ（sandbox-client の `DirEntry` に対応）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntryDto {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// フレーム読み書きのエラー。
#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    TooLarge(usize),
    Json(serde_json::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "io: {e}"),
            FrameError::TooLarge(n) => write!(f, "frame too large: {n}"),
            FrameError::Json(e) => write!(f, "json: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        FrameError::Io(e)
    }
}

/// 値を JSON 化し u32-LE 長さ前置でフレームを組む。
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let body = serde_json::to_vec(value).map_err(FrameError::Json)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(body.len()));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// 同期 writer へ 1 フレーム書く。
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, value: &T) -> Result<(), FrameError> {
    let frame = encode(value)?;
    w.write_all(&frame)?;
    w.flush()?;
    Ok(())
}

/// 同期 reader から 1 フレーム読む（EOF は `Ok(None)`）。
pub fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(
    r: &mut R,
) -> Result<Option<T>, FrameError> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(FrameError::Io(e)),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body).map_err(FrameError::Json)?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let reqs = [
            Request::Exec {
                argv: vec!["python3".into(), "-c".into(), "print(1)".into()],
                timeout_ms: 5000,
            },
            Request::WriteFile {
                path: "/workspace/a".into(),
                b64: "aGk=".into(),
            },
            Request::ReadFile {
                path: "/workspace/a".into(),
            },
            Request::ListDir {
                path: "/workspace".into(),
            },
            Request::Shutdown,
        ];
        for r in reqs {
            let mut buf = Vec::new();
            write_frame(&mut buf, &r).unwrap();
            let mut cur = std::io::Cursor::new(buf);
            let got: Request = read_frame(&mut cur).unwrap().unwrap();
            assert_eq!(got, r);
        }
    }

    #[test]
    fn event_roundtrip() {
        let ev = Event::Dir {
            entries: vec![DirEntryDto {
                name: "x".into(),
                is_dir: false,
                size: 3,
            }],
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &ev).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        assert_eq!(read_frame::<_, Event>(&mut cur).unwrap().unwrap(), ev);
    }

    #[test]
    fn multiple_frames_sequential() {
        let mut buf = Vec::new();
        write_frame(
            &mut buf,
            &Event::Ready {
                version: "1".into(),
            },
        )
        .unwrap();
        write_frame(&mut buf, &Event::Stdout { b64: "YQ==".into() }).unwrap();
        write_frame(&mut buf, &Event::Exited { code: 0 }).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        assert!(matches!(
            read_frame::<_, Event>(&mut cur).unwrap().unwrap(),
            Event::Ready { .. }
        ));
        assert!(matches!(
            read_frame::<_, Event>(&mut cur).unwrap().unwrap(),
            Event::Stdout { .. }
        ));
        assert!(matches!(
            read_frame::<_, Event>(&mut cur).unwrap().unwrap(),
            Event::Exited { code: 0 }
        ));
        assert!(read_frame::<_, Event>(&mut cur).unwrap().is_none());
    }

    #[test]
    fn eof_returns_none() {
        let mut cur = std::io::Cursor::new(Vec::new());
        assert!(read_frame::<_, Event>(&mut cur).unwrap().is_none());
    }

    #[test]
    fn oversize_len_rejected() {
        let mut buf = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(b"{}");
        let mut cur = std::io::Cursor::new(buf);
        assert!(matches!(
            read_frame::<_, Event>(&mut cur),
            Err(FrameError::TooLarge(_))
        ));
    }
}

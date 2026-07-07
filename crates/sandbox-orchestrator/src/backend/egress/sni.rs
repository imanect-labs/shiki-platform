//! 接続の先頭バイトからホスト名を取り出す（TLS ClientHello の SNI / HTTP/1.x の Host）。
//!
//! ゲスト由来の敵対的入力として扱い、境界チェックを厳密に行う（オーバーフロー/長さ詐称で panic しない）。
//! 何も取れなければ `None`（プロキシは遮断する）。

/// TLS ClientHello（record type 0x16）から SNI ホスト名を抽出する。
#[must_use]
pub(super) fn parse_tls_sni(buf: &[u8]) -> Option<String> {
    // TLS record: content_type(1)=0x16, version(2), length(2), fragment...
    let rec = buf;
    if rec.len() < 5 || rec[0] != 0x16 {
        return None;
    }
    let rec_len = u16::from_be_bytes([rec[3], rec[4]]) as usize;
    let hs = rec.get(5..5 + rec_len).unwrap_or(&rec[5..]);
    // Handshake: msg_type(1)=0x01 client_hello, length(3), body...
    if hs.len() < 4 || hs[0] != 0x01 {
        return None;
    }
    let mut p = 4usize; // skip type+len
                        // client_version(2) + random(32)
    p = p.checked_add(2 + 32)?;
    // session_id
    let sid_len = *hs.get(p)? as usize;
    p = p.checked_add(1 + sid_len)?;
    // cipher_suites: u16 len
    let cs_len = read_u16(hs, p)? as usize;
    p = p.checked_add(2 + cs_len)?;
    // compression_methods: u8 len
    let comp_len = *hs.get(p)? as usize;
    p = p.checked_add(1 + comp_len)?;
    // extensions: u16 len
    let ext_total = read_u16(hs, p)? as usize;
    p = p.checked_add(2)?;
    let ext_end = p.checked_add(ext_total)?;
    let ext_end = ext_end.min(hs.len());
    while p + 4 <= ext_end {
        let ext_type = read_u16(hs, p)?;
        let ext_len = read_u16(hs, p + 2)? as usize;
        let body_start = p + 4;
        let body_end = body_start.checked_add(ext_len)?;
        if body_end > ext_end {
            return None;
        }
        if ext_type == 0x0000 {
            return parse_sni_extension(hs.get(body_start..body_end)?);
        }
        p = body_end;
    }
    None
}

/// server_name 拡張の中身から host_name(type 0) を取り出す。
fn parse_sni_extension(body: &[u8]) -> Option<String> {
    // server_name_list: u16 len, then entries: type(1), name_len(2), name
    let _list_len = read_u16(body, 0)? as usize;
    let mut p = 2usize;
    while p + 3 <= body.len() {
        let name_type = *body.get(p)?;
        let name_len = read_u16(body, p + 1)? as usize;
        let name_start = p + 3;
        let name_end = name_start.checked_add(name_len)?;
        let name = body.get(name_start..name_end)?;
        if name_type == 0x00 {
            let host = std::str::from_utf8(name).ok()?;
            if is_valid_hostname(host) {
                return Some(host.to_ascii_lowercase());
            }
            return None;
        }
        p = name_end;
    }
    None
}

/// HTTP/1.x リクエストの Host ヘッダを抽出する（`Host: example.com[:port]`）。
///
/// **バイト列のまま**処理する: 敵対的入力（非 ASCII の Host・同一パケットにバイナリ本文）でも
/// `str` スライスの char 境界 panic を起こさず、ヘッダ領域だけを対象にする。
#[must_use]
pub(super) fn parse_http_host(buf: &[u8]) -> Option<String> {
    // ヘッダ領域のみ（最初の CRLFCRLF まで）。本文にバイナリが来ても壊れない。
    let head_end = find_subsequence(buf, b"\r\n\r\n").unwrap_or(buf.len());
    let head = &buf[..head_end];
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        // `Host:` を大文字小文字無視で（バイト比較）。
        if line.len() >= 5 && line[..5].eq_ignore_ascii_case(b"host:") {
            let v = trim_ascii(&line[5..]);
            // ポートを落とす（`host:port`）。
            let host_bytes = v.split(|&b| b == b':').next().unwrap_or(v);
            let host = std::str::from_utf8(trim_ascii(host_bytes)).ok()?;
            if is_valid_hostname(host) {
                return Some(host.to_ascii_lowercase());
            }
            return None;
        }
    }
    None
}

/// 部分列の開始オフセットを探す（ヘッダ終端 CRLFCRLF 検出用）。
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// 前後の ASCII 空白を除く（バイト列）。
fn trim_ascii(mut b: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = b {
        if first.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = b {
        if last.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
}

fn read_u16(buf: &[u8], at: usize) -> Option<u16> {
    let b = buf.get(at..at + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

/// ホスト名の健全性（allowlist 照合・DNS 応答に使う前の最小検証）。
fn is_valid_hostname(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小の ClientHello を SNI 付きで組み立てる。
    fn client_hello_with_sni(host: &str) -> Vec<u8> {
        let name = host.as_bytes();
        // server_name entry
        let mut sn_entry = Vec::new();
        sn_entry.push(0x00); // host_name
        sn_entry.extend_from_slice(&(name.len() as u16).to_be_bytes());
        sn_entry.extend_from_slice(name);
        // server_name_list
        let mut sn_list = Vec::new();
        sn_list.extend_from_slice(&(sn_entry.len() as u16).to_be_bytes());
        sn_list.extend_from_slice(&sn_entry);
        // extension: type 0x0000, len, body(sn_list)
        let mut ext = Vec::new();
        ext.extend_from_slice(&0x0000u16.to_be_bytes());
        ext.extend_from_slice(&(sn_list.len() as u16).to_be_bytes());
        ext.extend_from_slice(&sn_list);
        // extensions block
        let mut exts = Vec::new();
        exts.extend_from_slice(&(ext.len() as u16).to_be_bytes());
        exts.extend_from_slice(&ext);
        // handshake body
        let mut body = Vec::new();
        body.extend_from_slice(&[0x03, 0x03]); // version
        body.extend_from_slice(&[0u8; 32]); // random
        body.push(0x00); // session_id len
        body.extend_from_slice(&0x0002u16.to_be_bytes()); // cipher suites len
        body.extend_from_slice(&[0x13, 0x01]); // one suite
        body.push(0x01); // compression len
        body.push(0x00); // null compression
        body.extend_from_slice(&exts);
        // handshake header
        let mut hs = Vec::new();
        hs.push(0x01); // client_hello
        let blen = body.len();
        hs.extend_from_slice(&[(blen >> 16) as u8, (blen >> 8) as u8, blen as u8]);
        hs.extend_from_slice(&body);
        // record header
        let mut rec = Vec::new();
        rec.push(0x16);
        rec.extend_from_slice(&[0x03, 0x01]);
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[test]
    fn extracts_sni() {
        let ch = client_hello_with_sni("api.example.com");
        assert_eq!(parse_tls_sni(&ch).as_deref(), Some("api.example.com"));
    }

    #[test]
    fn sni_lowercased() {
        let ch = client_hello_with_sni("API.Example.COM");
        assert_eq!(parse_tls_sni(&ch).as_deref(), Some("api.example.com"));
    }

    #[test]
    fn rejects_non_tls() {
        assert_eq!(parse_tls_sni(b"GET / HTTP/1.1\r\n"), None);
        assert_eq!(parse_tls_sni(&[0x16, 0x03]), None);
    }

    #[test]
    fn truncated_does_not_panic() {
        let ch = client_hello_with_sni("api.example.com");
        for n in 0..ch.len() {
            let _ = parse_tls_sni(&ch[..n]); // panic しなければ良い
        }
    }

    #[test]
    fn http_host() {
        let req = b"GET / HTTP/1.1\r\nHost: example.com\r\nUser-Agent: x\r\n\r\n";
        assert_eq!(parse_http_host(req).as_deref(), Some("example.com"));
    }

    #[test]
    fn http_host_with_port_and_case() {
        let req = b"GET / HTTP/1.1\r\nhOsT: Example.com:8080\r\n\r\n";
        assert_eq!(parse_http_host(req).as_deref(), Some("example.com"));
    }

    #[test]
    fn rejects_bad_hostname() {
        let req = b"GET / HTTP/1.1\r\nHost: bad_host!\r\n\r\n";
        assert_eq!(parse_http_host(req), None);
    }

    #[test]
    fn multibyte_header_does_not_panic() {
        // 非 ASCII（マルチバイト）を含む Host は panic せず None（無効ホスト名）。
        let mut req = b"GET / HTTP/1.1\r\nHost: ".to_vec();
        req.extend_from_slice("日本語.example".as_bytes());
        req.extend_from_slice(b"\r\n\r\n");
        assert_eq!(parse_http_host(&req), None);
        // 先頭に短いマルチバイト行が来ても境界 panic しない。
        let weird = "Ho日: x\r\nHost: ok.example\r\n\r\n".as_bytes();
        assert_eq!(parse_http_host(weird).as_deref(), Some("ok.example"));
    }

    #[test]
    fn short_request_and_binary_body() {
        // 64 バイト未満の正当な HTTP でも Host を取れる。
        let short = b"GET / HTTP/1.1\r\nHost: a.co\r\n\r\n"; // ~30 bytes
        assert_eq!(parse_http_host(short).as_deref(), Some("a.co"));
        // 同一パケットにバイナリ本文が続いても UTF-8 変換で壊れない。
        let mut req = b"POST / HTTP/1.1\r\nHost: b.example\r\n\r\n".to_vec();
        req.extend_from_slice(&[0xff, 0xfe, 0x00, 0x80]);
        assert_eq!(parse_http_host(&req).as_deref(), Some("b.example"));
    }
}

//! 偽 DNS: netns 内の全 A クエリにゲートウェイ IP を返し、名前解決自体を外に出さない
//! （「DNS も egress」を構造的に閉じる）。AAAA/その他は NODATA(NOERROR) で A へ誘導する。
//!
//! ゲスト由来パケットは敵対的として扱う（長さ詐称・ラベルループで panic しない）。

use std::net::Ipv4Addr;

/// DNS クエリに対する応答を組み立てる（不正・非クエリなら `None`）。
#[must_use]
pub(super) fn build_response(query: &[u8], gateway: Ipv4Addr) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let flags = u16::from_be_bytes([query[2], query[3]]);
    // QR=1 は応答。クエリ(QR=0)のみ処理。
    if flags & 0x8000 != 0 {
        return None;
    }
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    if qdcount != 1 {
        return None;
    }
    // 質問セクション（qname）を読み切り、qtype/qclass を得る。
    let (qname_end, qtype) = parse_question(query, 12)?;

    let mut resp = Vec::with_capacity(query.len() + 16);
    // ヘッダ: id をコピー。
    resp.extend_from_slice(&query[0..2]);
    // flags: QR=1, opcode コピー, AA=1, TC=0, RD コピー, RA=1, RCODE=0。
    let opcode = flags & 0x7800;
    let rd = flags & 0x0100;
    let out_flags = 0x8000 | opcode | 0x0400 | rd | 0x0080;
    resp.extend_from_slice(&out_flags.to_be_bytes());
    resp.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    let is_a = qtype == 1;
    let ancount: u16 = u16::from(is_a);
    resp.extend_from_slice(&ancount.to_be_bytes());
    resp.extend_from_slice(&0u16.to_be_bytes()); // nscount
    resp.extend_from_slice(&0u16.to_be_bytes()); // arcount
                                                 // 質問セクションをそのままコピー。
    resp.extend_from_slice(query.get(12..qname_end + 4)?);
    if is_a {
        // 回答: name=圧縮ポインタ(0xC00C=offset 12), type A, class IN, ttl 30, rdlen 4, rdata。
        resp.extend_from_slice(&[0xC0, 0x0C]);
        resp.extend_from_slice(&1u16.to_be_bytes()); // type A
        resp.extend_from_slice(&1u16.to_be_bytes()); // class IN
        resp.extend_from_slice(&30u32.to_be_bytes()); // ttl
        resp.extend_from_slice(&4u16.to_be_bytes()); // rdlength
        resp.extend_from_slice(&gateway.octets());
    }
    Some(resp)
}

/// 質問セクションの qname を走査し、(qname 終端 offset, qtype) を返す。
fn parse_question(buf: &[u8], start: usize) -> Option<(usize, u16)> {
    let mut p = start;
    let mut guard = 0;
    loop {
        let len = *buf.get(p)? as usize;
        guard += 1;
        if guard > 128 {
            return None; // ラベル暴走
        }
        if len == 0 {
            p += 1;
            break;
        }
        // 圧縮ポインタは質問では通常出ない。安全側で拒否。
        if len & 0xC0 != 0 {
            return None;
        }
        p = p.checked_add(1 + len)?;
        if p > buf.len() {
            return None;
        }
    }
    let qtype = u16::from_be_bytes([*buf.get(p)?, *buf.get(p + 1)?]);
    // p は qtype の開始位置（qname の null 直後）。質問セクションは start..(p+4)＝qname＋qtype(2)＋qclass(2)。
    // 呼び出し側が `query[12..qend+4]` で丸ごとコピーするため qend=p を返す（以前の p-1 は qclass 末尾を欠落）。
    Some((p, qtype))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `www.example.com` の A クエリを組む。
    fn a_query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = Vec::new();
        q.extend_from_slice(&0x1234u16.to_be_bytes()); // id
        q.extend_from_slice(&0x0100u16.to_be_bytes()); // flags: RD
        q.extend_from_slice(&1u16.to_be_bytes()); // qdcount
        q.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // an/ns/ar
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes()); // class IN
        q
    }

    #[test]
    fn a_query_answered_with_gateway() {
        let gw: Ipv4Addr = "169.254.0.1".parse().unwrap();
        let query = a_query("www.example.com", 1);
        let resp = build_response(&query, gw).expect("resp");
        // header ancount = 1
        assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 1);
        // 質問セクション（qname＋qtype＋qclass）が欠落なく echo される（P1: qclass 末尾）。
        assert_eq!(
            &resp[12..query.len()],
            &query[12..],
            "question section echoed verbatim"
        );
        // 末尾 4 バイトが gateway。
        let n = resp.len();
        assert_eq!(&resp[n - 4..], &gw.octets());
        // QR=1
        assert_ne!(u16::from_be_bytes([resp[2], resp[3]]) & 0x8000, 0);
    }

    #[test]
    fn aaaa_is_nodata() {
        let gw: Ipv4Addr = "169.254.0.1".parse().unwrap();
        let resp = build_response(&a_query("www.example.com", 28), gw).expect("resp");
        assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 0); // ancount 0
    }

    #[test]
    fn rejects_response_and_garbage() {
        let gw: Ipv4Addr = "169.254.0.1".parse().unwrap();
        assert!(build_response(&[], gw).is_none());
        let mut resp_flag = a_query("x.com", 1);
        resp_flag[2] = 0x80; // QR=1
        assert!(build_response(&resp_flag, gw).is_none());
    }

    #[test]
    fn malformed_qname_does_not_panic() {
        let gw: Ipv4Addr = "169.254.0.1".parse().unwrap();
        // qdcount=1 だが qname が途中で切れる。
        let mut q = a_query("www.example.com", 1);
        q.truncate(15);
        let _ = build_response(&q, gw);
    }
}

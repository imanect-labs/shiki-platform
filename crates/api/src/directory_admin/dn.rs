//! AD distinguishedName のOUをRFC4514のエスケープを保って読む。

fn split_unescaped(value: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut escaped = false;
    let mut start = 0;
    for (index, ch) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
        } else if ch == separator {
            parts.push(&value[start..index]);
            start = index + ch.len_utf8();
        }
    }
    parts.push(&value[start..]);
    parts
}

fn decode(value: &str) -> Result<String, &'static str> {
    let bytes = value.as_bytes();
    let mut result = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index += 1;
            if index == bytes.len() {
                return Err("DNのエスケープが不正です");
            }
            if index + 1 < bytes.len()
                && bytes[index].is_ascii_hexdigit()
                && bytes[index + 1].is_ascii_hexdigit()
            {
                let hex = &value[index..index + 2];
                result.push(u8::from_str_radix(hex, 16).map_err(|_| "DNのエスケープが不正です")?);
                index += 2;
            } else {
                result.push(bytes[index]);
                index += 1;
            }
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }
    let decoded = String::from_utf8(result).map_err(|_| "DNがUTF-8ではありません")?;
    if decoded.contains('\0') || decoded.trim().is_empty() {
        return Err("DNの値が不正です");
    }
    Ok(decoded)
}

pub fn ou_path(dn: &str) -> Result<Vec<String>, &'static str> {
    let mut result = Vec::new();
    for rdn in split_unescaped(dn, ',') {
        let mut ous = Vec::new();
        for attribute in split_unescaped(rdn.trim_start(), '+') {
            let (kind, value) = attribute.split_once('=').ok_or("DNの属性が不正です")?;
            if kind.trim().eq_ignore_ascii_case("ou") {
                ous.push(decode(value)?);
            }
        }
        if ous.len() > 1 {
            return Err("複数値OUの階層を一意に決められません");
        }
        result.extend(ous);
    }
    result.reverse();
    Ok(result)
}

/// ADのcase-ignore DNをRDN単位で比較する。表記の異なるエスケープも同じキーにする。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key(Vec<Vec<(String, String)>>);
impl Key {
    pub fn within(&self, base: &Self) -> bool {
        self.0.ends_with(&base.0)
    }
}
pub fn key(dn: &str) -> Result<Key, &'static str> {
    let mut rdns = Vec::new();
    for rdn in split_unescaped(dn, ',') {
        let mut attributes = Vec::new();
        for attribute in split_unescaped(rdn.trim_start(), '+') {
            let (kind, value) = attribute.split_once('=').ok_or("DNの属性が不正です")?;
            let kind = kind.trim().to_ascii_lowercase();
            if kind.is_empty() {
                return Err("DNの属性が不正です");
            }
            attributes.push((kind, decode(value)?.to_lowercase()));
        }
        attributes.sort();
        rdns.push(attributes);
    }
    Ok(Key(rdns))
}
pub fn common_name(dn: &str) -> Result<String, &'static str> {
    let rdn = split_unescaped(dn, ',')
        .into_iter()
        .next()
        .ok_or("DNが空です")?;
    for attribute in split_unescaped(rdn, '+') {
        let (kind, value) = attribute.split_once('=').ok_or("DNの属性が不正です")?;
        if kind.trim().eq_ignore_ascii_case("cn") {
            return decode(value);
        }
    }
    Err("ADグループのCNがありません")
}
#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]
    use super::*;
    #[test]
    fn nested_ou_escapes_and_utf8() {
        assert_eq!(
            ou_path(
                r"CN=SG\,Sales,OU=\E5\96\B6\E6\A5\AD\E9\83\A8,OU=Tokyo\+East,DC=example,DC=com"
            )
            .unwrap(),
            vec!["Tokyo+East", "営業部"]
        );
        assert_eq!(
            ou_path(r"CN=A,OU=\ Leading\ ,DC=example").unwrap(),
            vec![" Leading "]
        );
        assert_eq!(
            key(r"CN=Smith\,Jeff,OU=East,DC=example").unwrap(),
            key(r"cn=smith\2Cjeff,ou=east,dc=EXAMPLE").unwrap()
        );
        assert_eq!(
            common_name(r"CN=Smith\,Jeff,OU=East").unwrap(),
            "Smith,Jeff"
        );
        assert!(key("broken").is_err());
        assert!(ou_path(r"CN=A,OU=bad\").is_err());
        assert!(ou_path("CN=A,OU=one+OU=two,DC=example").is_err());
    }
}

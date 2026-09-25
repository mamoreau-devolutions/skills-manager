//! JavaScript URI helpers: `encodeURIComponent`, `decodeURIComponent`,
//! `URLSearchParams` serialization, and WHATWG `URL` accessors.

use url::Url;

/// `encodeURIComponent`
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// `decodeURIComponent`; `Err` for malformed sequences (JS `URIError`).
#[allow(clippy::result_unit_err)]
pub fn decode_uri_component(s: &str) -> Result<String, ()> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3).ok_or(())?).map_err(|_| ())?;
            if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(());
            }
            let v = u8::from_str_radix(hex, 16).map_err(|_| ())?;
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

/// `new URLSearchParams(pairs).toString()`
pub fn search_params(pairs: &[(&str, &str)]) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(k, v);
    }
    ser.finish()
}

pub fn parse(s: &str) -> Option<Url> {
    Url::parse(s).ok()
}

/// WHATWG `url.protocol` (e.g. `https:`)
pub fn protocol(u: &Url) -> String {
    format!("{}:", u.scheme())
}

/// WHATWG `url.hostname`
pub fn hostname(u: &Url) -> String {
    match u.host() {
        Some(url::Host::Ipv6(a)) => format!("[{}]", a),
        Some(h) => h.to_string(),
        None => String::new(),
    }
}

/// WHATWG `url.host` (hostname plus non-default port)
pub fn host(u: &Url) -> String {
    let h = hostname(u);
    match u.port() {
        Some(p) => format!("{}:{}", h, p),
        None => h,
    }
}

/// WHATWG `url.pathname`
pub fn pathname(u: &Url) -> String {
    u.path().to_string()
}

pub fn search_param(u: &Url, name: &str) -> Option<String> {
    u.query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// `new URL(relative, base).toString()`
pub fn join(base: &str, relative: &str) -> Option<String> {
    Url::parse(base)
        .ok()?
        .join(relative)
        .ok()
        .map(|u| u.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode() {
        assert_eq!(encode_uri_component("a b/c@d"), "a%20b%2Fc%40d");
        assert_eq!(encode_uri_component("é"), "%C3%A9");
        assert_eq!(decode_uri_component("a%20b").unwrap(), "a b");
        assert!(decode_uri_component("%E0%A4%A").is_err());
        assert!(decode_uri_component("%zz").is_err());
        assert_eq!(
            search_params(&[("q", "a b"), ("x", "1&2")]),
            "q=a+b&x=1%262"
        );
    }

    #[test]
    fn url_accessors() {
        let u = parse("https://Example.com:8443/a/b?x=1").unwrap();
        assert_eq!(protocol(&u), "https:");
        assert_eq!(hostname(&u), "example.com");
        assert_eq!(host(&u), "example.com:8443");
        assert_eq!(pathname(&u), "/a/b");
        let d = parse("https://example.com").unwrap();
        assert_eq!(pathname(&d), "/");
    }
}

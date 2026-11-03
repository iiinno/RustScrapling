//! Port of upstream `scrapling.core.shell.CurlParser` (Scrapling v0.5,
//! BSD-3-Clause): turns a curl command copied from the DevTools network tab
//! into a request description. Supported flags are exactly upstream's; any
//! other flag is rejected with upstream's "Unknown/Unsupported" message
//! instead of being ignored.

use serde_json::{json, Map, Value};

/// Upstream `Request` namedtuple.
#[derive(Clone, Debug, PartialEq)]
pub struct CurlRequest {
    pub method: String,
    pub url: String,
    pub params: Vec<(String, String)>,
    /// A string body, or an object for `--data-urlencode` form fields.
    pub data: Option<Value>,
    pub json_data: Option<Value>,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<(String, String)>,
    pub proxy: Option<String>,
}

impl CurlRequest {
    pub fn to_json(&self) -> Value {
        let pairs = |items: &[(String, String)]| {
            Value::Object(
                items
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect::<Map<_, _>>(),
            )
        };
        json!({
            "method": self.method,
            "url": self.url,
            "params": pairs(&self.params),
            "data": self.data,
            "json_data": self.json_data,
            "headers": pairs(&self.headers),
            "cookies": pairs(&self.cookies),
            "proxy": self.proxy.as_ref().map(|p| json!({"http": p, "https": p})),
            "follow_redirects": "safe",
        })
    }
}

/// POSIX `shlex.split` (Python, `posix=True`, no comments).
pub fn shlex_split(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            '\'' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => current.push(ch),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(ch @ ('"' | '\\' | '$' | '`')) => current.push(ch),
                            Some('\n') => {}
                            Some(ch) => {
                                current.push('\\');
                                current.push(ch);
                            }
                            None => return Err("No closing quotation".into()),
                        },
                        Some(ch) => current.push(ch),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '\\' => {
                in_token = true;
                match chars.next() {
                    Some(ch) => current.push(ch),
                    None => return Err("No escaped character".into()),
                }
            }
            c => {
                in_token = true;
                current.push(c);
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    Ok(tokens)
}

#[derive(Default)]
struct Parsed {
    positionals: Vec<String>,
    method: Option<String>,
    headers: Vec<String>,
    user_agent: Option<String>,
    data: Option<String>,
    data_raw: Option<String>,
    data_binary: Option<String>,
    data_urlencode: Vec<String>,
    get: bool,
    cookie: Option<String>,
    proxy: Option<String>,
    proxy_user: Option<String>,
}

/// `(long, short, takes_value)` for every flag upstream declares.
const FLAGS: &[(&str, Option<char>, bool)] = &[
    ("--request", Some('X'), true),
    ("--header", Some('H'), true),
    ("--user-agent", Some('A'), true),
    ("--data", Some('d'), true),
    ("--data-raw", None, true),
    ("--data-binary", None, true),
    ("--data-urlencode", None, true),
    ("--get", Some('G'), false),
    ("--cookie", Some('b'), true),
    ("--proxy", Some('x'), true),
    ("--proxy-user", Some('U'), true),
    ("--insecure", Some('k'), false),
    ("--compressed", None, false),
    ("--include", Some('i'), false),
    ("--silent", Some('s'), false),
    ("--verbose", Some('v'), false),
];

fn assign(parsed: &mut Parsed, long: &str, value: Option<String>) {
    match (long, value) {
        ("--request", v) => parsed.method = v,
        ("--header", Some(v)) => parsed.headers.push(v),
        ("--user-agent", v) => parsed.user_agent = v,
        ("--data", v) => parsed.data = v,
        ("--data-raw", v) => parsed.data_raw = v,
        ("--data-binary", v) => parsed.data_binary = v,
        ("--data-urlencode", Some(v)) => parsed.data_urlencode.push(v),
        ("--get", _) => parsed.get = true,
        ("--cookie", v) => parsed.cookie = v,
        ("--proxy", v) => parsed.proxy = v,
        ("--proxy-user", v) => parsed.proxy_user = v,
        _ => {}
    }
}

fn python_list(items: &[String]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|item| format!("'{}'", item.replace('\\', "\\\\").replace('\'', "\\'")))
        .collect();
    format!("[{}]", quoted.join(", "))
}

fn parse_args(tokens: Vec<String>) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    let mut unknown = Vec::new();
    let mut iter = tokens.into_iter();
    let mut only_positionals = false;
    while let Some(token) = iter.next() {
        if only_positionals || token == "-" || !token.starts_with('-') {
            parsed.positionals.push(token);
            continue;
        }
        if token == "--" {
            only_positionals = true;
            continue;
        }
        if let Some(rest) = token.strip_prefix("--") {
            let (name, inline) = match rest.split_once('=') {
                Some((name, value)) => (format!("--{name}"), Some(value.to_owned())),
                None => (token.clone(), None),
            };
            match FLAGS.iter().find(|(long, _, _)| *long == name) {
                Some((long, _, true)) => {
                    let value = match inline {
                        Some(value) => value,
                        None => iter.next().ok_or_else(|| {
                            format!("Curl arguments parsing error: argument {long}: expected one argument")
                        })?,
                    };
                    assign(&mut parsed, long, Some(value));
                }
                Some((long, _, false)) if inline.is_none() => assign(&mut parsed, long, None),
                _ => unknown.push(token),
            }
            continue;
        }
        // Short options: `-XPOST`, `-X POST` and combined flags like `-sk`.
        let mut chars: Vec<char> = token[1..].chars().collect();
        let mut consumed = false;
        while !chars.is_empty() {
            let letter = chars.remove(0);
            match FLAGS.iter().find(|(_, short, _)| *short == Some(letter)) {
                Some((long, _, true)) => {
                    let value = if chars.is_empty() {
                        iter.next().ok_or_else(|| {
                            format!("Curl arguments parsing error: argument -{letter}/{long}: expected one argument")
                        })?
                    } else {
                        chars.drain(..).collect()
                    };
                    assign(&mut parsed, long, Some(value));
                    consumed = true;
                }
                Some((long, _, false)) => assign(&mut parsed, long, None),
                None => {
                    if !consumed {
                        unknown.push(token.clone());
                    }
                    break;
                }
            }
        }
    }
    if parsed.positionals.is_empty() {
        return Err(
            "Curl arguments parsing error: the following arguments are required: url".into(),
        );
    }
    if parsed.positionals.len() > 2 {
        unknown.extend(parsed.positionals.drain(2..));
    }
    if !unknown.is_empty() {
        return Err(format!(
            "Unknown/Unsupported curl arguments: {}",
            python_list(&unknown)
        ));
    }
    Ok(parsed)
}

/// `http.cookies.SimpleCookie().load` for DevTools-style `a=1; b=2` strings.
pub fn parse_cookie_string(input: &str) -> Vec<(String, String)> {
    const ATTRIBUTES: [&str; 10] = [
        "expires",
        "path",
        "comment",
        "domain",
        "max-age",
        "secure",
        "httponly",
        "version",
        "samesite",
        "partitioned",
    ];
    let mut cookies: Vec<(String, String)> = Vec::new();
    for part in input.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || ATTRIBUTES.contains(&key.to_ascii_lowercase().as_str()) {
            continue;
        }
        let mut value = value.trim().to_owned();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            value = value[1..value.len() - 1].to_owned();
        }
        match cookies.iter_mut().find(|(k, _)| k == key) {
            Some(existing) => existing.1 = value,
            None => cookies.push((key.to_owned(), value)),
        }
    }
    cookies
}

/// Ordered name/value pairs (header or cookie lists).
pub type Pairs = Vec<(String, String)>;

/// Upstream `_ParseHeaders`: "Key: Value" lines into headers and (when
/// `parse_cookies`) a `Cookie:` header into cookies; "Key;" is an empty header.
pub fn parse_header_lines(lines: &[String], parse_cookies: bool) -> Result<(Pairs, Pairs), String> {
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut cookies: Vec<(String, String)> = Vec::new();
    for line in lines {
        match line.split_once(':') {
            None if line.ends_with(';') => upsert(
                &mut headers,
                line[..line.len() - 1].trim().to_owned(),
                String::new(),
            ),
            None => return Err(format!("Could not parse header without colon: '{line}'.")),
            Some((key, value)) => {
                let (key, value) = (key.trim(), value.trim());
                if parse_cookies && key.eq_ignore_ascii_case("cookie") {
                    cookies = parse_cookie_string(value);
                } else {
                    upsert(&mut headers, key.to_owned(), value.to_owned());
                }
            }
        }
    }
    Ok((headers, cookies))
}

/// `urllib.parse.parse_qsl(.., keep_blank_values=True)`.
pub fn parse_qsl(query: &str) -> Vec<(String, String)> {
    fn decode(text: &str) -> String {
        let bytes = text.replace('+', " ").into_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(byte) =
                    u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
                {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(part), String::new()),
        })
        .collect()
}

pub fn upsert(items: &mut Vec<(String, String)>, key: String, value: String) {
    match items.iter_mut().find(|(k, _)| *k == key) {
        Some(existing) => existing.1 = value,
        None => items.push((key, value)),
    }
}

/// Upstream `CurlParser.parse`.
pub fn parse_curl(command: &str) -> Result<CurlRequest, String> {
    let clean = command
        .trim()
        .trim_start_matches(['c', 'u', 'r', 'l'])
        .trim()
        .replace("\\\n", " ");
    let tokens = shlex_split(&clean).map_err(|e| format!("Could not split command line: {e}"))?;
    let parsed = parse_args(tokens)?;
    let url = parsed.positionals.last().cloned().unwrap_or_default();

    let has_data = parsed.data.as_deref().is_some_and(|d| !d.is_empty())
        || parsed.data_raw.as_deref().is_some_and(|d| !d.is_empty())
        || parsed.data_binary.as_deref().is_some_and(|d| !d.is_empty())
        || !parsed.data_urlencode.is_empty();
    let method = if parsed.get {
        "get".to_owned()
    } else if let Some(method) = parsed.method.as_deref().filter(|m| !m.is_empty()) {
        method.trim().to_ascii_lowercase()
    } else if has_data {
        "post".to_owned()
    } else {
        "get".to_owned()
    };

    let (mut headers, mut cookies) = parse_header_lines(&parsed.headers, true)?;
    if let Some(agent) = &parsed.user_agent {
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
        {
            headers.push(("User-Agent".into(), agent.clone()));
        }
    }
    if let Some(cookie) = &parsed.cookie {
        for (key, value) in parse_cookie_string(cookie) {
            upsert(&mut cookies, key, value);
        }
    }

    // Precedence: --data-binary > --data-raw > -d > --data-urlencode.
    enum Payload {
        /// `--data-binary` is bytes upstream: never re-read as JSON.
        Binary(String),
        Text(String),
        Form(Vec<(String, String)>),
    }
    let mut payload = if let Some(binary) = parsed.data_binary {
        Some(Payload::Binary(binary))
    } else if let Some(raw) = parsed.data_raw {
        Some(Payload::Text(raw.trim_start_matches('$').to_owned()))
    } else if let Some(data) = parsed.data {
        Some(Payload::Text(data))
    } else if !parsed.data_urlencode.is_empty() {
        Some(Payload::Form(parse_qsl(&parsed.data_urlencode.join("&"))))
    } else {
        None
    };
    let mut json_data = None;
    if let Some(Payload::Text(text)) = &payload {
        if let Ok(value @ (Value::Object(_) | Value::Array(_))) =
            serde_json::from_str::<Value>(text)
        {
            json_data = Some(value);
            payload = None;
        }
    }
    let mut params = Vec::new();
    let non_empty = match &payload {
        Some(Payload::Text(text) | Payload::Binary(text)) => !text.is_empty(),
        Some(Payload::Form(form)) => !form.is_empty(),
        None => false,
    };
    if method == "get" && non_empty {
        match &payload {
            Some(Payload::Form(form)) => params.extend(form.iter().cloned()),
            Some(Payload::Text(text) | Payload::Binary(text)) => {
                for (k, v) in parse_qsl(text) {
                    upsert(&mut params, k, v);
                }
            }
            None => {}
        }
        if !params.is_empty() {
            payload = None;
            json_data = None;
        }
    }
    let data = payload.map(|payload| match payload {
        Payload::Text(text) | Payload::Binary(text) => Value::String(text),
        Payload::Form(form) => {
            let mut object = Map::new();
            for (k, v) in form {
                object.insert(k, Value::String(v));
            }
            Value::Object(object)
        }
    });

    let proxy = parsed.proxy.as_ref().map(|proxy| {
        let mut url = if proxy.contains("://") {
            proxy.clone()
        } else {
            format!("http://{proxy}")
        };
        if let Some(user) = &parsed.proxy_user {
            if let Some((scheme, rest)) = url.split_once("://") {
                let (netloc, tail) = match rest.find('/') {
                    Some(index) => rest.split_at(index),
                    None => (rest, ""),
                };
                let host = netloc.rsplit('@').next().unwrap_or(netloc);
                url = format!("{scheme}://{user}@{host}{tail}");
            }
        }
        url
    });

    Ok(CurlRequest {
        method,
        url,
        params,
        data,
        json_data,
        headers,
        cookies,
        proxy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devtools_command_round_trip() {
        let request = parse_curl(
            "curl 'https://example.com/api?x=1' \\\n  -H 'accept: application/json' \\\n  -H 'cookie: a=1; b=\"two\"' \\\n  -b 'c=3' \\\n  --data-raw $'{\"q\":1}' \\\n  --compressed",
        )
        .unwrap();
        assert_eq!(request.method, "post");
        assert_eq!(request.url, "https://example.com/api?x=1");
        assert_eq!(request.json_data, Some(json!({"q":1})));
        assert_eq!(request.data, None);
        assert_eq!(
            request.cookies,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "two".into()),
                ("c".into(), "3".into())
            ]
        );
        assert_eq!(
            request.headers,
            vec![("accept".into(), "application/json".into())]
        );
    }

    #[test]
    fn get_flag_moves_data_to_params_and_proxy_user_is_inserted() {
        let request =
            parse_curl("curl -G https://e.test -d 'a=1&b=x+y' -x proxy:8080 -U me:pw -A ua")
                .unwrap();
        assert_eq!(request.method, "get");
        assert_eq!(
            request.params,
            vec![("a".into(), "1".into()), ("b".into(), "x y".into())]
        );
        assert_eq!(request.data, None);
        assert_eq!(request.proxy.as_deref(), Some("http://me:pw@proxy:8080"));
        assert_eq!(request.headers, vec![("User-Agent".into(), "ua".into())]);
    }

    #[test]
    fn unknown_flags_and_missing_url_are_errors() {
        assert_eq!(
            parse_curl("curl https://e.test --http2 -L").unwrap_err(),
            "Unknown/Unsupported curl arguments: ['--http2', '-L']"
        );
        assert!(parse_curl("curl -s").unwrap_err().contains("required: url"));
        assert!(parse_curl("curl 'unterminated").is_err());
    }
}

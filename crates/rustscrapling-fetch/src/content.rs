use crate::{is_challenge, FetchError, FetchErrorKind, FetchResponse, FetchTier};
use encoding_rs::{Encoding, UTF_8};
use rustscrapling_dom::html5_backend::{parse_html, Html5Options};
use rustscrapling_extract::{extract, ExtractOptions, ExtractedPage};
use rustscrapling_http::HttpResponse;

/// Borrowed acquisition metadata. Browser tiers contain UTF-8 DOM, not HTTP bytes.
pub struct ContentInput<'a> {
    status: u16,
    headers: &'a [(String, String)],
    body: &'a [u8],
    tier: FetchTier,
    url: &'a str,
}

pub struct DecodedContent {
    pub source: String,
    pub extracted: Option<ExtractedPage>,
}

impl<'a> From<&'a HttpResponse> for ContentInput<'a> {
    fn from(response: &'a HttpResponse) -> Self {
        Self {
            status: response.status,
            headers: &response.headers,
            body: &response.body,
            tier: FetchTier::Http,
            url: &response.final_url,
        }
    }
}

impl<'a> From<&'a FetchResponse> for ContentInput<'a> {
    fn from(response: &'a FetchResponse) -> Self {
        Self {
            status: response.status,
            headers: &response.headers,
            body: &response.body,
            tier: response.tier,
            url: &response.final_url,
        }
    }
}

impl ContentInput<'_> {
    fn failure(&self, kind: FetchErrorKind, message: &str) -> FetchError {
        FetchError {
            kind,
            message: message.into(),
            hint: "Inspect the source response and its declared encoding".into(),
            status: Some(self.status),
        }
    }

    fn content_type(&self) -> &str {
        self.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map_or("", |(_, value)| value.as_str())
    }

    fn browser_dom(&self) -> bool {
        matches!(self.tier, FetchTier::Browser | FetchTier::Headed)
    }

    /// Decode without replacement characters. A browser has already applied HTTP encoding.
    pub fn decode_source(&self) -> Result<String, FetchError> {
        if self.browser_dom() {
            return std::str::from_utf8(self.body)
                .map(str::to_owned)
                .map_err(|_| {
                    self.failure(FetchErrorKind::DecodeFailed, "Browser DOM is not UTF-8")
                });
        }
        let (encoding, skip) = if let Some((encoding, skip)) = Encoding::for_bom(self.body) {
            (encoding, skip)
        } else {
            let label = charset_parameter(self.content_type()).or_else(|| {
                if self.content_type().to_ascii_lowercase().contains("html")
                    || looks_html(self.body)
                {
                    html_charset(self.body)
                } else {
                    None
                }
            });
            let encoding = match label {
                Some(label) => Encoding::for_label(label.as_bytes()).ok_or_else(|| {
                    self.failure(FetchErrorKind::DecodeFailed, "Unsupported declared charset")
                })?,
                None => UTF_8,
            };
            (encoding, 0)
        };
        encoding
            .decode_without_bom_handling_and_without_replacement(&self.body[skip..])
            .map(|text| text.into_owned())
            .ok_or_else(|| {
                self.failure(
                    FetchErrorKind::DecodeFailed,
                    "Body does not match its declared charset",
                )
            })
    }

    pub fn extract(&self, query: Option<&str>) -> Result<DecodedContent, FetchError> {
        if matches!(self.status, 401 | 403 | 429) || is_challenge(self.body) {
            return Err(self.failure(FetchErrorKind::Blocked, "Source refused the request"));
        }
        if !(200..300).contains(&self.status) {
            return Err(self.failure(
                FetchErrorKind::HttpStatus,
                "Source returned a non-success status",
            ));
        }
        let source = self.decode_source()?;
        if is_challenge(source.as_bytes()) {
            return Err(self.failure(FetchErrorKind::Blocked, "Source returned a challenge"));
        }
        let html = self.browser_dom()
            || self.content_type().to_ascii_lowercase().contains("html")
            || looks_html(source.as_bytes());
        let extracted = if html {
            Some(
                extract(
                    &source,
                    ExtractOptions {
                        page_url: Some(self.url),
                        query,
                    },
                )
                .map_err(|_| {
                    self.failure(FetchErrorKind::DecodeFailed, "HTML extraction failed")
                })?,
            )
        } else {
            None
        };
        let text = extracted
            .as_ref()
            .map_or(source.as_str(), |page| page.markdown.as_str());
        if text.trim().is_empty() {
            let kind = if html && !self.browser_dom() && has_executable_script(&source) {
                FetchErrorKind::NeedsRendering
            } else {
                FetchErrorKind::Empty
            };
            return Err(self.failure(kind, "Source has no usable content"));
        }
        Ok(DecodedContent { source, extracted })
    }
}

fn charset_parameter(value: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|part| {
        let (name, value) = part.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['\'', '"']).trim().to_owned())
    })
}

fn looks_html(body: &[u8]) -> bool {
    let prefix = &body[..body.len().min(256)];
    let text = String::from_utf8_lossy(prefix)
        .trim_start()
        .to_ascii_lowercase();
    text.starts_with("<!doctype html") || text.starts_with("<html")
}

fn html_charset(body: &[u8]) -> Option<String> {
    // Only ASCII declaration labels are inspected before the actual strict decode.
    let prefix = String::from_utf8_lossy(&body[..body.len().min(1024)]);
    let dom = parse_html(&prefix, &Html5Options::default()).ok()?;
    for id in dom.descendants_or_self(dom.root) {
        if dom.node(id).tag != "meta" {
            continue;
        }
        if let Some(label) = dom.attr(id, "charset") {
            return Some(label.trim().to_owned());
        }
        if dom
            .attr(id, "http-equiv")
            .is_some_and(|v| v.eq_ignore_ascii_case("content-type"))
        {
            if let Some(label) = dom.attr(id, "content").and_then(charset_parameter) {
                return Some(label);
            }
        }
    }
    None
}

fn has_executable_script(source: &str) -> bool {
    let Ok(dom) = parse_html(source, &Html5Options::default()) else {
        return false;
    };
    dom.descendants_or_self(dom.root).into_iter().any(|id| {
        if dom.node(id).tag != "script" {
            return false;
        }
        let kind = dom
            .attr(id, "type")
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        matches!(
            kind.as_str(),
            "" | "module" | "text/javascript" | "application/javascript"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(body: Vec<u8>, content_type: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            final_url: "https://example.test/article".into(),
            headers: vec![("Content-Type".into(), content_type.into())],
            body,
            http_version: "HTTP/1.1".into(),
            attempts: 1,
            http3_attempted: false,
        }
    }

    #[test]
    fn utf8_bom_precedes_http_declaration() {
        let body = response(
            "\u{feff}研究🦀".as_bytes().to_vec(),
            "text/plain; charset=shift_jis",
        );
        assert_eq!(ContentInput::from(&body).decode_source().unwrap(), "研究🦀");
    }

    #[test]
    fn commented_meta_is_not_a_charset_declaration() {
        let body = response("<html><head><!-- <meta charset='shift_jis'> --></head><body><p>研究🦀</p></body></html>".as_bytes().to_vec(), "text/html");
        let parsed = ContentInput::from(&body).extract(None).unwrap();
        assert_eq!(parsed.extracted.unwrap().markdown.trim(), "研究🦀");
    }

    #[test]
    fn unsupported_html_charset_is_explicit_failure() {
        let body = response(
            b"<html><head><meta charset='x-unknown'></head><body>content</body></html>".to_vec(),
            "text/html",
        );
        let error = ContentInput::from(&body).decode_source().unwrap_err();
        assert_eq!(error.kind, FetchErrorKind::DecodeFailed);
        assert_eq!(error.status, Some(200));
    }

    #[test]
    fn json_ld_alone_is_empty_not_a_renderable_shell() {
        let body = response(b"<html><head><script type='application/ld+json'>{\"name\":\"metadata\"}</script></head><body></body></html>".to_vec(), "text/html");
        let result = ContentInput::from(&body).extract(None);
        assert!(matches!(
            result,
            Err(FetchError {
                kind: FetchErrorKind::Empty,
                ..
            })
        ));
    }
}

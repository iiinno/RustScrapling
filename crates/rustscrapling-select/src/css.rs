//! CSS selector engine compatible with the subset of cssselect that
//! Scrapling exposes (cssselect is what lxml's `cssselect()` uses).
//!
//! Supported: type and universal selectors, `#id`, `.class`, attribute
//! selectors (`=`, `~=`, `|=`, `^=`, `$=`, `*=`, presence), the four
//! combinators, `:first-child`, `:last-child`, `:only-child`,
//! `:first-of-type`, `:last-of-type`, `:only-of-type`, `:nth-child()`,
//! `:nth-last-child()`, `:nth-of-type()`, `:nth-last-of-type()`,
//! `:empty`, `:root`, `:not()`, `:has()`, `:contains()` (cssselect
//! extension), `:lang()`, `:enabled`, `:disabled`, `:checked`, `:link`,
//! and the Scrapling pseudo-elements `::text` and `::attr(name)`.
//!
//! Unknown pseudo-classes are a syntax error, matching cssselect
//! (for example `:hidden` raises SelectorSyntaxError in Scrapling).

use crate::{syntax, Result, Selection};
use rustscrapling_dom::{Dom, NodeId, NodeKind};

#[derive(Debug, Clone, PartialEq)]
enum Pseudo {
    Text,
    Attr(String),
}

#[derive(Debug, Clone, PartialEq)]
enum AttrOp {
    Exists,
    Eq,
    Includes,
    DashMatch,
    Prefix,
    Suffix,
    Substring,
}

#[derive(Debug, Clone, PartialEq)]
struct AttrCond {
    name: String,
    op: AttrOp,
    value: String,
}

#[derive(Debug, Clone, PartialEq)]
enum PseudoClass {
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    Nth {
        a: i64,
        b: i64,
        last: bool,
        of_type: bool,
    },
    Empty,
    Root,
    Not(Vec<ComplexSelector>),
    Has(Vec<(Combinator, ComplexSelector)>),
    Contains(String),
    Lang(String),
    Enabled,
    Disabled,
    Checked,
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Combinator {
    Descendant,
    Child,
    Adjacent,
    Sibling,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<AttrCond>,
    pseudos: Vec<PseudoClass>,
}

#[derive(Debug, Clone, PartialEq)]
struct ComplexSelector {
    /// Left to right. The first combinator is a placeholder.
    parts: Vec<(Combinator, Compound)>,
}

pub fn css(dom: &Dom, scope: NodeId, selector: &str) -> Result<Vec<Selection>> {
    let (groups, pseudo) = parse(selector)?;
    let mut out: Vec<Selection> = Vec::new();
    let mut seen: Vec<NodeId> = Vec::new();
    for cand in candidates(dom, scope) {
        for g in &groups {
            if matches_complex(dom, cand, g) {
                if !seen.contains(&cand) {
                    seen.push(cand);
                    match &pseudo {
                        None => out.push(Selection::Element(cand)),
                        Some(Pseudo::Text) => {
                            for &child in dom.children(cand) {
                                let n = dom.node(child);
                                if n.kind == NodeKind::Text {
                                    out.push(Selection::Text(n.data.clone()));
                                }
                            }
                        }
                        Some(Pseudo::Attr(name)) => {
                            if let Some(v) = dom.attr(cand, name) {
                                out.push(Selection::Text(v.to_string()));
                            }
                        }
                    }
                }
                break;
            }
        }
    }
    Ok(out)
}

/// Elements under `scope` (including scope itself when it is an element
/// in the document tree) in document order.
fn candidates(dom: &Dom, scope: NodeId) -> Vec<NodeId> {
    dom.descendants_or_self(scope)
        .into_iter()
        .filter(|&n| dom.node(n).is_element())
        .collect()
}

fn matches_complex(dom: &Dom, el: NodeId, sel: &ComplexSelector) -> bool {
    match_from(dom, el, sel, sel.parts.len() - 1)
}

fn match_from(dom: &Dom, el: NodeId, sel: &ComplexSelector, idx: usize) -> bool {
    let (comb, comp) = &sel.parts[idx];
    if !matches_compound(dom, el, comp) {
        return false;
    }
    if idx == 0 {
        return true;
    }
    match comb {
        Combinator::Child => match dom.parent(el) {
            Some(p) if dom.node(p).is_element() => match_from(dom, p, sel, idx - 1),
            _ => false,
        },
        Combinator::Descendant => {
            let mut cur = dom.parent(el);
            while let Some(p) = cur {
                if dom.node(p).is_element() && match_from(dom, p, sel, idx - 1) {
                    return true;
                }
                cur = dom.parent(p);
            }
            false
        }
        Combinator::Adjacent => match dom.prev_element_sibling(el) {
            Some(s) => match_from(dom, s, sel, idx - 1),
            None => false,
        },
        Combinator::Sibling => {
            let mut cur = dom.prev_element_sibling(el);
            while let Some(s) = cur {
                if match_from(dom, s, sel, idx - 1) {
                    return true;
                }
                cur = dom.prev_element_sibling(s);
            }
            false
        }
    }
}

fn matches_compound(dom: &Dom, el: NodeId, comp: &Compound) -> bool {
    let node = dom.node(el);
    if let Some(tag) = &comp.tag {
        if tag != "*" && node.tag != *tag {
            return false;
        }
    }
    if let Some(id) = &comp.id {
        if dom.attr(el, "id") != Some(id.as_str()) {
            return false;
        }
    }
    for class in &comp.classes {
        let classes = dom.attr(el, "class").unwrap_or("");
        if !classes.split_whitespace().any(|c| c == class) {
            return false;
        }
    }
    for cond in &comp.attrs {
        let value = dom.attr(el, &cond.name);
        let ok = match (&cond.op, value) {
            (AttrOp::Exists, Some(_)) => true,
            (AttrOp::Exists, None) => false,
            (_, None) => false,
            (AttrOp::Eq, Some(v)) => v == cond.value,
            (AttrOp::Includes, Some(v)) => v.split_whitespace().any(|w| w == cond.value),
            (AttrOp::DashMatch, Some(v)) => {
                v == cond.value || v.starts_with(&format!("{}-", cond.value))
            }
            (AttrOp::Prefix, Some(v)) => !cond.value.is_empty() && v.starts_with(&cond.value),
            (AttrOp::Suffix, Some(v)) => !cond.value.is_empty() && v.ends_with(&cond.value),
            (AttrOp::Substring, Some(v)) => !cond.value.is_empty() && v.contains(&cond.value),
        };
        if !ok {
            return false;
        }
    }
    for pseudo in &comp.pseudos {
        if !matches_pseudo(dom, el, pseudo) {
            return false;
        }
    }
    true
}

fn sibling_index(dom: &Dom, el: NodeId, of_type: bool, last: bool) -> Option<(usize, usize)> {
    let sibs: Vec<NodeId> = if of_type {
        let tag = &dom.node(el).tag;
        dom.element_siblings(el)
            .into_iter()
            .filter(|&s| &dom.node(s).tag == tag)
            .collect()
    } else {
        dom.element_siblings(el)
    };
    let pos = sibs.iter().position(|&s| s == el)?;
    let idx = if last { sibs.len() - pos } else { pos + 1 };
    Some((idx, sibs.len()))
}

fn nth_matches(a: i64, b: i64, idx: usize) -> bool {
    let idx = idx as i64;
    if a == 0 {
        return idx == b;
    }
    let diff = idx - b;
    diff % a == 0 && diff / a >= 0
}

fn matches_pseudo(dom: &Dom, el: NodeId, pseudo: &PseudoClass) -> bool {
    match pseudo {
        PseudoClass::FirstChild => matches!(sibling_index(dom, el, false, false), Some((1, _))),
        PseudoClass::LastChild => matches!(sibling_index(dom, el, false, true), Some((1, _))),
        PseudoClass::OnlyChild => matches!(sibling_index(dom, el, false, false), Some((_, 1))),
        PseudoClass::FirstOfType => matches!(sibling_index(dom, el, true, false), Some((1, _))),
        PseudoClass::LastOfType => matches!(sibling_index(dom, el, true, true), Some((1, _))),
        PseudoClass::OnlyOfType => matches!(sibling_index(dom, el, true, false), Some((_, 1))),
        PseudoClass::Nth {
            a,
            b,
            last,
            of_type,
        } => match sibling_index(dom, el, *of_type, *last) {
            Some((idx, _)) => nth_matches(*a, *b, idx),
            None => false,
        },
        PseudoClass::Empty => dom.children(el).iter().all(|&c| {
            let n = dom.node(c);
            !(n.is_element() || (n.kind == NodeKind::Text && !n.data.is_empty()))
        }),
        PseudoClass::Root => dom.document_element() == Some(el),
        PseudoClass::Not(groups) => !groups.iter().any(|g| matches_complex(dom, el, g)),
        PseudoClass::Has(sels) => sels.iter().any(|(comb, g)| {
            let pool: Vec<NodeId> = match comb {
                Combinator::Child => dom.element_children(el),
                Combinator::Adjacent => dom.next_element_sibling(el).into_iter().collect(),
                Combinator::Sibling => {
                    let sibs = dom.element_siblings(el);
                    let pos = sibs.iter().position(|&s| s == el).unwrap_or(0);
                    sibs.into_iter().skip(pos + 1).collect()
                }
                Combinator::Descendant => dom
                    .descendants_or_self(el)
                    .into_iter()
                    .filter(|&n| n != el && dom.node(n).is_element())
                    .collect(),
            };
            pool.iter().any(|&cand| {
                // The relative selector's leftmost compound anchors at the
                // scope element; remaining parts match from the candidate.
                if g.parts.len() == 1 {
                    matches_compound(dom, cand, &g.parts[0].1)
                } else {
                    matches_complex(dom, cand, g)
                }
            })
        }),
        PseudoClass::Contains(text) => dom.text_content(el).contains(text.as_str()),
        PseudoClass::Lang(lang) => {
            let mut cur = Some(el);
            while let Some(n) = cur {
                if let Some(v) = dom.attr(n, "lang").or_else(|| dom.attr(n, "xml:lang")) {
                    return v == lang || v.starts_with(&format!("{}-", lang));
                }
                cur = dom.parent(n).filter(|&p| dom.node(p).is_element());
            }
            false
        }
        PseudoClass::Enabled => {
            matches!(
                dom.node(el).tag.as_str(),
                "button" | "input" | "select" | "textarea" | "optgroup" | "option"
            ) && dom.attr(el, "disabled").is_none()
        }
        PseudoClass::Disabled => {
            matches!(
                dom.node(el).tag.as_str(),
                "button" | "input" | "select" | "textarea" | "optgroup" | "option" | "fieldset"
            ) && dom.attr(el, "disabled").is_some()
        }
        PseudoClass::Checked => {
            dom.attr(el, "checked").is_some()
                || (dom.node(el).tag == "option" && dom.attr(el, "selected").is_some())
        }
        PseudoClass::Link => {
            matches!(dom.node(el).tag.as_str(), "a" | "area" | "link")
                && dom.attr(el, "href").is_some()
        }
    }
}

// ---------- parser ----------

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

fn parse(selector: &str) -> Result<(Vec<ComplexSelector>, Option<Pseudo>)> {
    let mut p = Parser {
        src: selector.as_bytes(),
        pos: 0,
    };
    let mut groups = Vec::new();
    let mut pseudo = None;
    p.skip_ws();
    loop {
        let (complex, ps) = p.parse_complex()?;
        if ps.is_some() {
            if pseudo.is_some() && pseudo != ps {
                return syntax("multiple different pseudo-elements");
            }
            pseudo = ps;
        }
        groups.push(complex);
        p.skip_ws();
        if p.peek() == Some(b',') {
            p.pos += 1;
            p.skip_ws();
            continue;
        }
        break;
    }
    if p.pos != p.src.len() {
        return syntax(format!("unexpected trailing input at byte {}", p.pos));
    }
    if groups.is_empty() {
        return syntax("empty selector");
    }
    Ok((groups, pseudo))
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_ws(&mut self) -> bool {
        let start = self.pos;
        while matches!(
            self.peek(),
            Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        ) {
            self.pos += 1;
        }
        self.pos > start
    }

    fn expect(&mut self, b: u8) -> Result<()> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            syntax(format!("expected '{}' at byte {}", b as char, self.pos))
        }
    }

    fn parse_ident(&mut self) -> Result<String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80 || c == b'\\' {
                self.pos += 1;
                if c == b'\\' {
                    // Escaped char, kept verbatim. A trailing backslash must
                    // not move past the end of the selector (found by fuzzing).
                    self.pos = (self.pos + 1).min(self.src.len());
                }
            } else {
                break;
            }
        }
        if self.pos == start {
            return syntax(format!("expected identifier at byte {}", start));
        }
        Ok(String::from_utf8_lossy(&self.src[start..self.pos]).replace('\\', ""))
    }

    fn parse_string_or_ident(&mut self) -> Result<String> {
        match self.peek() {
            Some(b'"') | Some(b'\'') => {
                let quote = self.peek().unwrap();
                self.pos += 1;
                let mut out = String::new();
                while let Some(c) = self.peek() {
                    if c == quote {
                        self.pos += 1;
                        return Ok(out);
                    }
                    if c == b'\\' {
                        self.pos += 1;
                        if let Some(n) = self.peek() {
                            out.push(n as char);
                            self.pos += 1;
                        }
                        continue;
                    }
                    // Consume one UTF-8 codepoint worth of bytes.
                    let s = &self.src[self.pos..];
                    let ch_len = utf8_len(s[0]);
                    out.push_str(&String::from_utf8_lossy(&s[..ch_len.min(s.len())]));
                    self.pos += ch_len;
                }
                syntax("unterminated string")
            }
            _ => self.parse_ident(),
        }
    }

    fn parse_complex(&mut self) -> Result<(ComplexSelector, Option<Pseudo>)> {
        let mut parts: Vec<(Combinator, Compound)> = Vec::new();
        let mut pseudo = None;
        let (first, ps) = self.parse_compound()?;
        if ps.is_some() {
            pseudo = ps;
        }
        parts.push((Combinator::Descendant, first));
        loop {
            let had_ws = self.skip_ws();
            match self.peek() {
                Some(b'>') => {
                    self.pos += 1;
                    self.skip_ws();
                    let (c, ps) = self.parse_compound()?;
                    if ps.is_some() {
                        pseudo = ps;
                    }
                    parts.push((Combinator::Child, c));
                }
                Some(b'+') => {
                    self.pos += 1;
                    self.skip_ws();
                    let (c, ps) = self.parse_compound()?;
                    if ps.is_some() {
                        pseudo = ps;
                    }
                    parts.push((Combinator::Adjacent, c));
                }
                Some(b'~') => {
                    self.pos += 1;
                    self.skip_ws();
                    let (c, ps) = self.parse_compound()?;
                    if ps.is_some() {
                        pseudo = ps;
                    }
                    parts.push((Combinator::Sibling, c));
                }
                Some(b',') | None => break,
                Some(_) if had_ws => {
                    let (c, ps) = self.parse_compound()?;
                    if ps.is_some() {
                        pseudo = ps;
                    }
                    parts.push((Combinator::Descendant, c));
                }
                Some(c) => {
                    return syntax(format!(
                        "unexpected character '{}' at byte {}",
                        c as char, self.pos
                    ))
                }
            }
            if pseudo.is_some() {
                // A pseudo-element must be the last thing in its complex
                // selector; cssselect rejects anything after it.
                let save = self.pos;
                self.skip_ws();
                match self.peek() {
                    Some(b',') | None => {
                        self.pos = save;
                        break;
                    }
                    _ => return syntax("pseudo-element must come last"),
                }
            }
        }
        Ok((ComplexSelector { parts }, pseudo))
    }

    fn parse_compound(&mut self) -> Result<(Compound, Option<Pseudo>)> {
        let mut comp = Compound::default();
        let mut pseudo = None;
        let mut any = false;
        // Type or universal selector.
        match self.peek() {
            Some(b'*') => {
                self.pos += 1;
                comp.tag = Some("*".to_string());
                any = true;
            }
            Some(c) if is_ident_start(c) => {
                comp.tag = Some(self.parse_ident()?.to_ascii_lowercase());
                any = true;
            }
            _ => {}
        }
        loop {
            match self.peek() {
                Some(b'#') => {
                    self.pos += 1;
                    comp.id = Some(self.parse_ident()?);
                    any = true;
                }
                Some(b'.') => {
                    self.pos += 1;
                    comp.classes.push(self.parse_ident()?);
                    any = true;
                }
                Some(b'[') => {
                    comp.attrs.push(self.parse_attr()?);
                    any = true;
                }
                Some(b':') => {
                    if self.src.get(self.pos + 1) == Some(&b':') {
                        self.pos += 2;
                        let name = self.parse_ident()?.to_ascii_lowercase();
                        match name.as_str() {
                            "text" => pseudo = Some(Pseudo::Text),
                            "attr" => {
                                self.expect(b'(')?;
                                self.skip_ws();
                                let attr = self.parse_ident()?;
                                self.skip_ws();
                                self.expect(b')')?;
                                pseudo = Some(Pseudo::Attr(attr.to_ascii_lowercase()));
                            }
                            _ => return syntax(format!("unsupported pseudo-element ::{}", name)),
                        }
                        any = true;
                        break;
                    }
                    self.pos += 1;
                    let pc = self.parse_pseudo_class()?;
                    comp.pseudos.push(pc);
                    any = true;
                }
                _ => break,
            }
        }
        if !any {
            return syntax(format!("expected a selector at byte {}", self.pos));
        }
        Ok((comp, pseudo))
    }

    fn parse_attr(&mut self) -> Result<AttrCond> {
        self.expect(b'[')?;
        self.skip_ws();
        let name = self.parse_ident()?.to_ascii_lowercase();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(AttrCond {
                name,
                op: AttrOp::Exists,
                value: String::new(),
            });
        }
        let op = match (self.peek(), self.src.get(self.pos + 1)) {
            (Some(b'='), _) => {
                self.pos += 1;
                AttrOp::Eq
            }
            (Some(b'~'), Some(b'=')) => {
                self.pos += 2;
                AttrOp::Includes
            }
            (Some(b'|'), Some(b'=')) => {
                self.pos += 2;
                AttrOp::DashMatch
            }
            (Some(b'^'), Some(b'=')) => {
                self.pos += 2;
                AttrOp::Prefix
            }
            (Some(b'$'), Some(b'=')) => {
                self.pos += 2;
                AttrOp::Suffix
            }
            (Some(b'*'), Some(b'=')) => {
                self.pos += 2;
                AttrOp::Substring
            }
            _ => return syntax(format!("bad attribute operator at byte {}", self.pos)),
        };
        self.skip_ws();
        let value = self.parse_string_or_ident()?;
        self.skip_ws();
        // cssselect accepts a case-insensitivity flag `i` here.
        if matches!(
            self.peek(),
            Some(b'i') | Some(b'I') | Some(b's') | Some(b'S')
        ) {
            let save = self.pos;
            let _ = self.parse_ident();
            self.skip_ws();
            if self.peek() != Some(b']') {
                self.pos = save;
            }
        }
        self.expect(b']')?;
        Ok(AttrCond { name, op, value })
    }

    fn parse_pseudo_class(&mut self) -> Result<PseudoClass> {
        let name = self.parse_ident()?.to_ascii_lowercase();
        match name.as_str() {
            "first-child" => Ok(PseudoClass::FirstChild),
            "last-child" => Ok(PseudoClass::LastChild),
            "only-child" => Ok(PseudoClass::OnlyChild),
            "first-of-type" => Ok(PseudoClass::FirstOfType),
            "last-of-type" => Ok(PseudoClass::LastOfType),
            "only-of-type" => Ok(PseudoClass::OnlyOfType),
            "empty" => Ok(PseudoClass::Empty),
            "root" => Ok(PseudoClass::Root),
            "enabled" => Ok(PseudoClass::Enabled),
            "disabled" => Ok(PseudoClass::Disabled),
            "checked" => Ok(PseudoClass::Checked),
            "link" => Ok(PseudoClass::Link),
            "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
                let arg = self.parse_parens_raw()?;
                let (a, b) = parse_nth(&arg)?;
                Ok(PseudoClass::Nth {
                    a,
                    b,
                    last: name.contains("last"),
                    of_type: name.contains("of-type"),
                })
            }
            "not" => {
                self.expect(b'(')?;
                let inner = self.read_balanced_parens()?;
                let (groups, pseudo) = parse(&inner)?;
                if pseudo.is_some() {
                    return syntax("pseudo-element inside :not()");
                }
                Ok(PseudoClass::Not(groups))
            }
            "has" => {
                self.expect(b'(')?;
                let inner = self.read_balanced_parens()?;
                let sels = parse_relative(&inner)?;
                Ok(PseudoClass::Has(sels))
            }
            "contains" => {
                self.expect(b'(')?;
                self.skip_ws();
                let text = self.parse_string_or_ident()?;
                self.skip_ws();
                self.expect(b')')?;
                Ok(PseudoClass::Contains(text))
            }
            "lang" => {
                self.expect(b'(')?;
                self.skip_ws();
                let lang = self.parse_string_or_ident()?;
                self.skip_ws();
                self.expect(b')')?;
                Ok(PseudoClass::Lang(lang))
            }
            _ => syntax(format!("unsupported pseudo-class :{}", name)),
        }
    }

    fn parse_parens_raw(&mut self) -> Result<String> {
        self.expect(b'(')?;
        self.read_balanced_parens()
    }

    /// Reads until the matching close paren (the open paren is already
    /// consumed) and returns the inner text, consuming the close paren.
    fn read_balanced_parens(&mut self) -> Result<String> {
        let start = self.pos;
        let mut depth = 1usize;
        while let Some(c) = self.peek() {
            match c {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner =
                            String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
                        self.pos += 1;
                        return Ok(inner);
                    }
                }
                b'"' | b'\'' => {
                    let q = c;
                    self.pos += 1;
                    while let Some(c2) = self.peek() {
                        self.pos += 1;
                        if c2 == q {
                            break;
                        }
                    }
                    continue;
                }
                _ => {}
            }
            self.pos += 1;
        }
        syntax("unbalanced parentheses")
    }
}

fn parse_relative(inner: &str) -> Result<Vec<(Combinator, ComplexSelector)>> {
    // :has() argument: complex selectors, optionally starting with a
    // combinator (relative selector). Split top-level commas.
    let mut out = Vec::new();
    for part in split_top_level(inner) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (comb, rest) = match part.chars().next() {
            Some('>') => (Combinator::Child, part[1..].trim()),
            Some('+') => (Combinator::Adjacent, part[1..].trim()),
            Some('~') => (Combinator::Sibling, part[1..].trim()),
            _ => (Combinator::Descendant, part),
        };
        let (groups, pseudo) = parse(rest)?;
        if pseudo.is_some() {
            return syntax("pseudo-element inside :has()");
        }
        for g in groups {
            out.push((comb, g));
        }
    }
    if out.is_empty() {
        return syntax("empty :has()");
    }
    Ok(out)
}

fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '(' | '[' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' | ']' => {
                    depth -= 1;
                    cur.push(c);
                }
                ',' if depth == 0 => {
                    parts.push(cur.clone());
                    cur.clear();
                }
                _ => cur.push(c),
            },
        }
    }
    parts.push(cur);
    parts
}

fn parse_nth(arg: &str) -> Result<(i64, i64)> {
    let s: String = arg
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match s.as_str() {
        "odd" => return Ok((2, 1)),
        "even" => return Ok((2, 0)),
        _ => {}
    }
    if let Some(npos) = s.find('n') {
        let a_part = &s[..npos];
        let a: i64 = match a_part {
            "" | "+" => 1,
            "-" => -1,
            _ => a_part
                .parse()
                .map_err(|_| SelectError::Syntax(format!("bad nth: {}", arg)))?,
        };
        let b_part = &s[npos + 1..];
        let b: i64 = if b_part.is_empty() {
            0
        } else {
            b_part
                .parse()
                .map_err(|_| SelectError::Syntax(format!("bad nth: {}", arg)))?
        };
        Ok((a, b))
    } else {
        let b: i64 = s
            .parse()
            .map_err(|_| SelectError::Syntax(format!("bad nth: {}", arg)))?;
        Ok((0, b))
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'-' || c >= 0x80
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

use crate::SelectError;

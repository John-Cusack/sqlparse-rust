//! Grouped token tree + `TokenList` navigation semantics for sqlparse 0.6.0.
//!
//! Mirrors `sql.py` (`Token.match` identity + case rules, `_token_matching`
//! scan shape, `group_tokens` splice/graft) over an arena-free `Vec<Node>`
//! per level. Whitespace/comment/keyword predicates use `TType::is_in`
//! (prefix = upstream `in`); exact-type checks use `==` (upstream `is`).

use crate::lexer::RawTok;
use crate::tables_gen::TType;

pub const MAX_GROUPING_DEPTH: u32 = 100;
pub const MAX_GROUPING_TOKENS: usize = 10000;

#[derive(Debug)]
pub enum CoreError {
    Depth,
    Tokens,
}

impl CoreError {
    pub fn message(&self) -> String {
        match self {
            CoreError::Depth => {
                format!("Maximum grouping depth exceeded ({MAX_GROUPING_DEPTH}).")
            }
            CoreError::Tokens => {
                format!("Maximum number of tokens exceeded ({MAX_GROUPING_TOKENS}).")
            }
        }
    }
}

/// Group classes created by the grouping engine (mirrors `sql.py` names).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupKind {
    Statement,
    Identifier,
    IdentifierList,
    Function,
    Where,
    Having,
    Parenthesis,
    SquareBrackets,
    Comparison,
    TypedLiteral,
    Comment,
    Operation,
    Assignment,
    Values,
    Case,
    If,
    For,
    Begin,
    Over,
    TokenList,
}

impl GroupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GroupKind::Statement => "Statement",
            GroupKind::Identifier => "Identifier",
            GroupKind::IdentifierList => "IdentifierList",
            GroupKind::Function => "Function",
            GroupKind::Where => "Where",
            GroupKind::Having => "Having",
            GroupKind::Parenthesis => "Parenthesis",
            GroupKind::SquareBrackets => "SquareBrackets",
            GroupKind::Comparison => "Comparison",
            GroupKind::TypedLiteral => "TypedLiteral",
            GroupKind::Comment => "Comment",
            GroupKind::Operation => "Operation",
            GroupKind::Assignment => "Assignment",
            GroupKind::Values => "Values",
            GroupKind::Case => "Case",
            GroupKind::If => "If",
            GroupKind::For => "For",
            GroupKind::Begin => "Begin",
            GroupKind::Over => "Over",
            GroupKind::TokenList => "TokenList",
        }
    }
}

#[derive(Debug)]
pub enum Node {
    Tok { ttype: TType, start: u32, end: u32 },
    Grp { kind: GroupKind, kids: Vec<Node> },
}

impl Node {
    pub fn is_group(&self) -> bool {
        matches!(self, Node::Grp { .. })
    }

    pub fn leaf(ttype: TType, tok: &RawTok) -> Node {
        Node::Tok { ttype, start: tok.start, end: tok.end }
    }

    pub fn value<'a>(&self, text: &'a str) -> &'a str {
        match self {
            Node::Tok { start, end, .. } => &text[*start as usize..*end as usize],
            Node::Grp { .. } => "",
        }
    }
}

impl TType {
    /// Upstream `ttype in other` (prefix containment).
    pub fn is_in(self, other: TType) -> bool {
        if self == other {
            return true;
        }
        let (a, b) = (self.as_str(), other.as_str());
        a.len() > b.len() && a.starts_with(b) && a.as_bytes()[b.len()] == b'.'
    }
}
/// Case-insensitive string equality via full uppercase (matches Python
/// `str.upper()` comparison, including length-changing folds like ß→SS).
pub fn eq_ci_str(a: &str, b: &str) -> bool {
    a.chars().flat_map(|c| c.to_uppercase()).collect::<String>()
        == b.chars().flat_map(|c| c.to_uppercase()).collect::<String>()
}
pub fn is_ws(n: &Node) -> bool {
    matches!(n, Node::Tok { ttype, .. } if ttype.is_in(TType::TOKEN_TEXT_WHITESPACE))
}

pub fn is_nl(n: &Node) -> bool {
    matches!(n, Node::Tok { ttype, .. } if *ttype == TType::TOKEN_TEXT_WHITESPACE_NEWLINE)
}

pub fn is_kw(n: &Node) -> bool {
    matches!(n, Node::Tok { ttype, .. } if ttype.is_in(TType::TOKEN_KEYWORD))
}

pub fn is_cm(n: &Node) -> bool {
    match n {
        Node::Tok { ttype, .. } => ttype.is_in(TType::TOKEN_COMMENT),
        Node::Grp { kind, .. } => *kind == GroupKind::Comment,
    }
}

/// `normalized` equality: keywords compare case-insensitively, groups never
/// match, other tokens compare exactly.
pub fn norm_eq(text: &str, n: &Node, s: &str) -> bool {
    match n {
        Node::Tok { .. } if is_kw(n) => eq_ci_str(n.value(text), s),
        Node::Tok { .. } => n.value(text) == s,
        Node::Grp { .. } => false,
    }
}

/// Match values: `None` (type only), one string, or a tuple of strings.
pub enum MatchVal<'a> {
    None,
    One(&'a str),
    Many(&'a [&'a str]),
}

/// Mirrors `Token.match` (non-regex; regex only occurs in the
/// aligned-indent filter, ported with the format engine).
pub fn tok_match(text: &str, n: &Node, ttype: TType, values: MatchVal<'_>) -> bool {
    let t = match n {
        Node::Tok { ttype, .. } => *ttype,
        Node::Grp { .. } => return false,
    };
    if t != ttype {
        return false;
    }
    match values {
        MatchVal::None => true,
        MatchVal::One(v) => {
            if is_kw(n) {
                eq_ci_str(n.value(text), v)
            } else {
                n.value(text) == v
            }
        }
        MatchVal::Many(vs) => {
            if is_kw(n) {
                vs.iter().any(|v| eq_ci_str(n.value(text), v))
            } else {
                vs.iter().any(|v| n.value(text) == *v)
            }
        }
    }
}

/// `imt` filter arguments: instance kinds, (ttype, values) patterns
/// (single or list), ttypes (single or list).
pub enum ImtI {
    None,
    One(GroupKind),
    Many(&'static [GroupKind]),
}

pub enum ImtM<'a> {
    None,
    One(TType, MatchVal<'a>),
    Many(&'static [(TType, MatchVal<'static>)]),
}

pub enum ImtT {
    None,
    One(TType),
    Many(&'static [TType]),
}

/// Mirrors `utils.imt` (None-safe; instance, then match, then ttype).
pub fn imt(text: &str, n: Option<&Node>, i: ImtI, m: ImtM<'_>, t: ImtT) -> bool {
    let node = match n {
        Some(v) => v,
        None => return false,
    };
    match i {
        ImtI::One(k) => {
            if matches!(node, Node::Grp { kind, .. } if *kind == k) {
                return true;
            }
        }
        ImtI::Many(ks) => {
            if matches!(node, Node::Grp { kind, .. } if ks.contains(kind)) {
                return true;
            }
        }
        ImtI::None => {}
    }
    match m {
        ImtM::One(tt, ref v) => {
            if tok_match(text, node, tt, match_val_clone(v)) {
                return true;
            }
        }
        ImtM::Many(ps) => {
            for (tt, v) in ps {
                if tok_match(text, node, *tt, match_val_clone(v)) {
                    return true;
                }
            }
        }
        ImtM::None => {}
    }
    match t {
        ImtT::One(tt) => {
            if matches!(node, Node::Tok { ttype, .. } if ttype.is_in(tt)) {
                return true;
            }
        }
        ImtT::Many(tts) => {
            if matches!(node, Node::Tok { ttype, .. } if tts.iter().any(|tt| ttype.is_in(*tt))) {
                return true;
            }
        }
        ImtT::None => {}
    }
    false
}

fn match_val_clone<'a>(v: &MatchVal<'a>) -> MatchVal<'a> {
    match v {
        MatchVal::None => MatchVal::None,
        MatchVal::One(s) => MatchVal::One(s),
        MatchVal::Many(vs) => MatchVal::Many(vs),
    }
}

/// Forward scan from `start` over `kids` for the first node passing `f`.
/// Mirrors `_token_matching` (bare `None` start → `None`).
fn scan(
    kids: &[Node],
    f: &dyn Fn(&Node) -> bool,
    start: Option<usize>,
    end: Option<usize>,
) -> Option<usize> {
    let s = start?;
    let e = end.unwrap_or(kids.len()).min(kids.len());
    let s = s.min(e);
    kids[s..e].iter().position(f).map(|i| s + i)
}

/// Reverse scan below `start` (mirrors `range(start - 2, -1, -1)` after the
/// caller's pre-increment, i.e. strictly below `idx`).
fn scan_rev(kids: &[Node], f: &dyn Fn(&Node) -> bool, idx: usize) -> Option<usize> {
    (0..idx.min(kids.len())).rev().find(|&i| f(&kids[i]))
}

/// `token_next(idx)`: `None` in → `None`; else first non-ws at/after idx+1.
pub fn token_next(
    kids: &[Node],
    idx: Option<usize>,
    skip_ws: bool,
    skip_cm: bool,
) -> Option<usize> {
    let s = idx? + 1;
    scan(kids, &|n| !(skip_ws && is_ws(n) || skip_cm && is_cm(n)), Some(s), None)
}

/// `token_prev(idx)`: first non-ws strictly below idx.
pub fn token_prev(
    kids: &[Node],
    idx: usize,
    skip_ws: bool,
    skip_cm: bool,
) -> Option<usize> {
    scan_rev(kids, &|n| !(skip_ws && is_ws(n) || skip_cm && is_cm(n)), idx)
}

pub fn token_first(kids: &[Node], skip_ws: bool, skip_cm: bool) -> Option<usize> {
    scan(kids, &|n| !(skip_ws && is_ws(n) || skip_cm && is_cm(n)), Some(0), None)
}

pub fn token_next_by(
    text: &str,
    kids: &[Node],
    i: ImtI,
    m: ImtM<'_>,
    t: ImtT,
    idx: isize,
    end: Option<usize>,
) -> Option<usize> {
    let s = (idx + 1).max(0) as usize;
    scan(kids, &|n| imt(text, Some(n), ref_i(&i), ref_m(&m), ref_t(&t)), Some(s), end)
}

fn ref_i(i: &ImtI) -> ImtI {
    match i {
        ImtI::None => ImtI::None,
        ImtI::One(k) => ImtI::One(*k),
        ImtI::Many(ks) => ImtI::Many(ks),
    }
}

fn ref_m<'x>(m: &ImtM<'x>) -> ImtM<'x> {
    match m {
        ImtM::None => ImtM::None,
        ImtM::One(tt, v) => ImtM::One(*tt, match_val_clone(v)),
        ImtM::Many(ps) => ImtM::Many(ps),
    }
}

fn ref_t(t: &ImtT) -> ImtT {
    match t {
        ImtT::None => ImtT::None,
        ImtT::One(tt) => ImtT::One(*tt),
        ImtT::Many(tts) => ImtT::Many(tts),
    }
}

/// `token_not_matching(funcs, idx)`: first node at/after idx failing all.
pub fn token_not_matching(
    kids: &[Node],
    f: &dyn Fn(&Node) -> bool,
    idx: Option<usize>,
) -> Option<usize> {
    scan(kids, &|n| !f(n), idx, None)
}

/// Splice `kids[from..to]` into a new `kind` group, or graft onto an
/// existing adjacent group when `extend` (mirrors `group_tokens`,
/// `include_end=True` at all grouping call sites).
pub fn group_tokens(
    kids: &mut Vec<Node>,
    kind: GroupKind,
    from: usize,
    to: usize,
    extend: bool,
) {
    let end = to + 1;
    if extend && matches!(kids.get(from), Some(Node::Grp { kind: k, .. }) if *k == kind) {
        let mut tail = kids.split_off(end.min(kids.len()));
        let mut graft: Vec<Node> = kids.split_off(from + 1);
        if let Some(Node::Grp { kids: g, .. }) = kids.last_mut() {
            g.append(&mut graft);
        }
        kids.append(&mut tail);
    } else {
        let mut sub: Vec<Node> = kids.drain(from..end.min(kids.len())).collect();
        let _ = &mut sub;
        kids.insert(from, Node::Grp { kind, kids: sub });
    }
}

/// Groupable children (Parenthesis/SquareBrackets exclude the brackets).
pub fn groupable<'a>(kind: GroupKind, kids: &'a [Node]) -> &'a [Node] {
    match kind {
        GroupKind::Parenthesis | GroupKind::SquareBrackets if kids.len() >= 2 => {
            &kids[1..kids.len() - 1]
        }
        _ => kids,
    }
}

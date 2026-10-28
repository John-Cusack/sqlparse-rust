//! Format engine mirroring `filters/` + `formatter.build_filter_stack`.
//!
//! Owned, parented arena (`ANode`) so the mutating stmtprocess filters
//! (`StripComments`, `StripWhitespace`, `SpacesAroundOperators`,
//! `Reindent`, `AlignedIndent`) transcribe 1:1. Preprocess (case/truncate)
//! runs on flat tokens with the text rebuilt once; postprocess
//! (`OutputPython/PHP`, `SerializerUnicode`) renders from the arena.
//! `RightMarginFilter` stays a raising stub like upstream.

use crate::tables_gen::TType;
use crate::tree::{self, GroupKind};

#[derive(Debug)]
pub enum FormatError {
    Group(tree::CoreError),
    NotImplemented,
}

pub struct FormatOptions {
    pub keyword_case: u8,    // 0 none, 1 upper, 2 lower, 3 capitalize
    pub identifier_case: u8, // 0 none, 1 upper, 2 lower, 3 capitalize
    pub truncate: Option<(usize, String)>,
    pub space_ops: bool,
    pub strip_comments: bool,
    pub strip_ws: bool,
    pub reindent: bool,
    pub reindent_aligned: bool,
    pub indent_width: usize,
    pub indent_char: char,
    pub indent_after_first: bool,
    pub indent_columns: bool,
    pub wrap_after: usize,
    pub comma_first: bool,
    pub compact: bool,
    pub right_margin: Option<usize>,
    pub output: u8, // 0 sql/none, 1 python, 2 php
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NKind {
    Tok,
    Grp(GroupKind),
}

#[derive(Clone)]
struct ANode {
    ttype: Option<TType>,
    kind: NKind,
    text: String, // leaves: live value
    frozen: String, // groups: value frozen at build (+grafts); mirrors TokenList.value
    kids: Vec<usize>,
    parent: Option<usize>,
}

struct Arena {
    nodes: Vec<ANode>,
}

impl Arena {
    fn new() -> Arena {
        Arena { nodes: Vec::new() }
    }

    fn push(&mut self, n: ANode) -> usize {
        let id = self.nodes.len();
        self.nodes.push(n);
        id
    }

    fn leaf(&mut self, ttype: TType, text: String, parent: Option<usize>) -> usize {
        self.push(ANode { ttype: Some(ttype), kind: NKind::Tok, text, frozen: String::new(), kids: Vec::new(), parent })
    }

    fn group(&mut self, kind: GroupKind, parent: Option<usize>) -> usize {
        self.push(ANode { ttype: None, kind: NKind::Grp(kind), text: String::new(), frozen: String::new(), kids: Vec::new(), parent })
    }

    fn from_tree(&mut self, text: &str, kids: &[tree::Node], parent: Option<usize>) -> Vec<usize> {
        let mut out = Vec::with_capacity(kids.len());
        for k in kids {
            match k {
                tree::Node::Tok { ttype, start, end } => {
                    out.push(self.leaf(*ttype, text[*start as usize..*end as usize].to_owned(), parent));
                }
                tree::Node::Grp { kind, kids: sub, .. } => {
                    let id = self.group(*kind, parent);
                    let c = self.from_tree(text, sub, Some(id));
                    self.nodes[id].kids = c;
                    // Frozen at build (= Python's .value after grouping);
                    // later leaf mutations/inserts never update it.
                    let live = self.group_text_live(id);
                    self.nodes[id].frozen = live;
                    out.push(id);
                }
            }
        }
        out
    }

    fn is_group(&self, id: usize) -> bool {
        matches!(self.nodes[id].kind, NKind::Grp(_))
    }

    fn kind_is(&self, id: usize, k: GroupKind) -> bool {
        matches!(self.nodes[id].kind, NKind::Grp(kk) if kk == k)
    }

    fn is_ws(&self, id: usize) -> bool {
        match self.nodes[id].ttype {
            Some(t) => t.is_in(TType::TOKEN_TEXT_WHITESPACE),
            None => false,
        }
    }

    fn is_kw(&self, id: usize) -> bool {
        match self.nodes[id].ttype {
            Some(t) => t.is_in(TType::TOKEN_KEYWORD),
            None => false,
        }
    }

    fn is_cm(&self, id: usize) -> bool {
        match self.nodes[id].ttype {
            Some(t) => t.is_in(TType::TOKEN_COMMENT),
            None => self.kind_is(id, GroupKind::Comment),
        }
    }
    /// `imt` over arena nodes: group-kind match OR `m` (exact type +
    /// values, like `Token.match`) OR `t` (bare containment, like imt's
    /// t-branch: prefix, never values).
    fn imt_node(
        &self, id: usize, kinds: &[GroupKind],
        m: Option<(TType, MatchVal<'_>)>, t: Option<TType>,
    ) -> bool {
        if !kinds.is_empty()
            && matches!(self.nodes[id].kind, NKind::Grp(k) if kinds.contains(&k))
        {
            return true;
        }
        if let Some((tt, mv)) = m {
            if self.tok_match(id, tt, Some(mv)) {
                return true;
            }
        }
        if let Some(tt) = t {
            if matches!(self.nodes[id].ttype, Some(t) if t.is_in(tt)) {
                return true;
            }
        }
        false
    }

    /// First index after `start` whose node matches either ttype.
    fn next_by_either(
        &self, parent: usize, start: isize, t1: TType, t2: TType,
    ) -> Option<usize> {
        let kids = &self.nodes[parent].kids;
        let mut i = (start + 1).max(0) as usize;
        while i < kids.len() {
            let k = kids[i];
            if matches!(self.nodes[k].ttype, Some(t) if t == t1 || t == t2) {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn next_by(
        &self, parent: usize, start: isize, kinds: &[GroupKind],
        m: Option<(TType, MatchVal<'_>)>, t: Option<TType>,
    ) -> Option<usize> {
        let kids = &self.nodes[parent].kids;
        let mut i = (start + 1).max(0) as usize;
        while i < kids.len() {
            // MatchVal is Copy (all payloads are), so reborrow per probe.
            let mm = m.clone();
            if self.imt_node(kids[i], kinds, mm, t) {
                return Some(i);
            }
            i += 1;
        }
        None
    }
    fn render_into(&self, id: usize, out: &mut String) {
        if self.is_group(id) {
            let kids = self.nodes[id].kids.clone();
            for k in kids {
                self.render_into(k, out);
            }
        } else {
            out.push_str(&self.nodes[id].text);
        }
    }

    fn position(&self, parent: usize, id: usize) -> Option<usize> {
        self.nodes[parent].kids.iter().position(|&k| k == id)
    }

    fn next(&self, parent: usize, idx: usize, skip_ws: bool, skip_cm: bool) -> Option<usize> {
        self.nodes[parent].kids.iter().enumerate().skip(idx + 1).find_map(|(i, &k)| {
            if (skip_ws && self.is_ws(k)) || (skip_cm && self.is_cm(k)) {
                None
            } else {
                Some(i)
            }
        })
    }

    fn prev(&self, parent: usize, idx: usize, skip_ws: bool, skip_cm: bool) -> Option<usize> {
        let mut i = idx;
        while i > 0 {
            i -= 1;
            let k = self.nodes[parent].kids[i];
            if (skip_ws && self.is_ws(k)) || (skip_cm && self.is_cm(k)) {
                continue;
            }
            return Some(i);
        }
        None
    }

    fn norm_eq(&self, id: usize, s: &str) -> bool {
        if self.is_group(id) {
            return false;
        }
        if self.is_kw(id) {
            tree::eq_ci_str(&self.nodes[id].text, s)
        } else {
            self.nodes[id].text == *s
        }
    }

    fn tok_match(&self, id: usize, ttype: TType, values: Option<MatchVal<'_>>) -> bool {
        let t = match self.nodes[id].ttype {
            Some(t) => t,
            None => return false,
        };
        if t != ttype {
            return false;
        }
        match values {
            None => true,
            Some(MatchVal::One(v)) => {
                if self.is_kw(id) {
                    tree::eq_ci_str(&self.nodes[id].text, v)
                } else {
                    self.nodes[id].text == *v
                }
            }
        }
    }

    /// Flat-text of a node (live concat for groups, mirrors `str(token)`).
    fn group_text_live(&self, id: usize) -> String {
        let mut out = String::new();
        self.render_into(id, &mut out);
        out
    }

    /// Frozen group text (mirrors `TokenList.value`: join at creation, plus
    /// graft-appends; leaf mutations/inserts never update it). For leaves
    /// this is just the live text.
    fn group_value(&self, id: usize) -> String {
        if self.is_group(id) {
            self.nodes[id].frozen.clone()
        } else {
            self.nodes[id].text.clone()
        }
    }

    fn insert_at(&mut self, parent: usize, idx: usize, child: usize) {
        self.nodes[parent].kids.insert(idx, child);
        self.nodes[child].parent = Some(parent);
    }

    /// Mirrors `insert_after` (default `skip_ws=True`): insert before the
    /// next non-whitespace, or append when there is none.
    fn insert_after(&mut self, parent: usize, idx: usize, child: usize) {
        self.nodes[child].parent = Some(parent);
        match self.next(parent, idx, true, false) {
            Some(ni) => self.nodes[parent].kids.insert(ni, child),
            None => self.nodes[parent].kids.push(child),
        }
    }

    fn remove_at(&mut self, parent: usize, idx: usize) {
        self.nodes[parent].kids.remove(idx);
    }
}

#[derive(Clone)]
enum MatchVal<'a> {
    One(&'a str),
}

fn apply_case(s: &str, mode: u8) -> String {
    match mode {
        1 => s.chars().flat_map(|c| c.to_uppercase()).collect(),
        2 => s.chars().flat_map(|c| c.to_lowercase()).collect(),
        3 => {
            let mut ch = s.chars();
            match ch.next() {
                None => String::new(),
                Some(f) => {
                    f.to_uppercase().collect::<String>()
                        + &ch.flat_map(|c| c.to_lowercase()).collect::<String>()
                }
            }
        }
        _ => s.to_owned(),
    }
}

/// Preprocess on flat tokens (case filters + truncate), then rebuild the
/// text once so every downstream span stays valid.
fn preprocess(
    text: &str, toks: &[crate::lexer::RawTok], opts: &FormatOptions,
) -> (String, Vec<crate::lexer::RawTok>) {
    use crate::lexer::RawTok;
    let mut out_text = String::with_capacity(text.len());
    let mut out: Vec<RawTok> = Vec::with_capacity(toks.len());
    let mut off = 0u32;
    for t in toks {
        let v = &text[t.start as usize..t.end as usize];
        let mut val = if opts.keyword_case != 0 && t.ttype.is_in(TType::TOKEN_KEYWORD) {
            apply_case(v, opts.keyword_case)
        } else if opts.identifier_case != 0
            && (t.ttype == TType::TOKEN_NAME
                || t.ttype == TType::TOKEN_LITERAL_STRING_SYMBOL)
            && v.trim().chars().next() != Some('"')
        {
            // Upstream indexes `value.strip()[0]` (panics on all-ws, which
            // a Name/Symbol token can never be).
            apply_case(v, opts.identifier_case)
        } else {
            v.to_owned()
        };
        if let Some((w, ch)) = &opts.truncate {
            if t.ttype == TType::TOKEN_LITERAL_STRING_SINGLE {
                let chars: Vec<char> = val.chars().collect();
                let (inner, quote) = if chars.len() >= 2 && chars[0] == '\'' && chars[1] == '\'' {
                    (&chars[2..chars.len() - 2], "''")
                } else {
                    (&chars[1..chars.len() - 1], "'")
                };
                if inner.len() > *w {
                    let mut q = String::from(quote);
                    q.extend(inner[..*w].iter());
                    q.push_str(ch);
                    q.push_str(quote);
                    val = q;
                }
            }
        }
        let start = off;
        off += val.len() as u32;
        out_text.push_str(&val);
        out.push(RawTok { ttype: t.ttype, start, end: off });
    }
    (out_text, out)
}

fn ws_leaf(arena: &mut Arena, parent: usize, text: &str) -> usize {
    arena.leaf(TType::TOKEN_TEXT_WHITESPACE, text.to_owned(), Some(parent))
}

fn nl_leaf(arena: &mut Arena, parent: usize, text: &str) -> usize {
    arena.leaf(TType::TOKEN_TEXT_WHITESPACE_NEWLINE, text.to_owned(), Some(parent))
}

/// Mirrors `StripCommentsFilter` (per level, callers recurse sublists first).
fn strip_comments(arena: &mut Arena, parent: usize) {
    // Recurse into sublists first (snapshot ids; deeper passes never splice
    // this level's list).
    let subs: Vec<usize> = arena.nodes[parent].kids.clone().into_iter().filter(|&k| arena.is_group(k)).collect();
    for s in subs {
        strip_comments(arena, s);
    }
    let mut tidx = arena.next_by(parent, -1, &[GroupKind::Comment], None, Some(TType::TOKEN_COMMENT));
    while let Some(t) = tidx {
        let tok = arena.nodes[parent].kids[t];
        let is_hint = arena.nodes[tok].ttype.is_some_and(|tt| {
            tt == TType::TOKEN_COMMENT_MULTILINE_HINT || tt == TType::TOKEN_COMMENT_SINGLE_HINT
        }) || (arena.is_group(tok)
            && arena.nodes[tok].kids.first().is_some_and(|&c| {
                arena.nodes[c].ttype.is_some_and(|tt| {
                    tt == TType::TOKEN_COMMENT_MULTILINE_HINT || tt == TType::TOKEN_COMMENT_SINGLE_HINT
                })
            }));
        if is_hint {
            tidx = arena.next_by(parent, t as isize, &[GroupKind::Comment], None, Some(TType::TOKEN_COMMENT));
            continue;
        }
        let p = arena.prev(parent, t, false, false);
        let n = arena.next(parent, t, false, false);
        let pv = p.map(|i| arena.nodes[parent].kids[i]);
        let nx = n.map(|i| arena.nodes[parent].kids[i]);
        let cut = pv.is_none()
            || nx.is_none()
            || pv.is_some_and(|x| arena.is_ws(x))
            || pv.is_some_and(|x| arena.tok_match(x, TType::TOKEN_PUNCTUATION, Some(MatchVal::One("("))))
            || nx.is_some_and(|x| arena.is_ws(x))
            || nx.is_some_and(|x| arena.tok_match(x, TType::TOKEN_PUNCTUATION, Some(MatchVal::One(")"))));
        if cut {
            if let Some(pi) = p {
                let prev_id = arena.nodes[parent].kids[pi];
                if !arena.tok_match(prev_id, TType::TOKEN_PUNCTUATION, Some(MatchVal::One("("))) {
                    let ins = insert_token_for(arena, parent, tok);
                    arena.insert_at(parent, t, ins);
                    arena.remove_at(parent, t + 1);
                } else {
                    arena.remove_at(parent, t);
                }
            } else {
                arena.remove_at(parent, t);
            }
            tidx = if t == 0 { None } else { Some(t - 1) };
            tidx = match tidx {
                None => arena.next_by(parent, -1, &[GroupKind::Comment], None, Some(TType::TOKEN_COMMENT)),
                Some(i) => arena.next_by(parent, i as isize, &[GroupKind::Comment], None, Some(TType::TOKEN_COMMENT)),
            };
        } else {
            let ins = insert_token_for(arena, parent, tok);
            arena.nodes[parent].kids[t] = ins;
            arena.nodes[ins].parent = Some(parent);
            tidx = arena.next_by(parent, t as isize, &[GroupKind::Comment], None, Some(TType::TOKEN_COMMENT));
        }
    }
}

/// `_get_insert_token`: newline run preserved (issue484) else one space.
fn insert_token_for(arena: &mut Arena, parent: usize, tok: usize) -> usize {
    let v = arena.group_value(tok);
    let tail = v.trim_end_matches(' ');
    let nl_run: String = tail.chars().rev().take_while(|&c| c == '\r' || c == '\n').collect::<Vec<_>>().into_iter().rev().collect();
    if nl_run.is_empty() {
        ws_leaf(arena, parent, " ")
    } else {
        nl_leaf(arena, parent, &nl_run)
    }
}

/// Mirrors `StripWhitespaceFilter` (per level + depth-0 trailing pop).
fn strip_whitespace(arena: &mut Arena, parent: usize, depth: u32) {
    let subs: Vec<usize> = arena.nodes[parent].kids.clone().into_iter().filter(|&k| arena.is_group(k)).collect();
    for s in subs {
        strip_whitespace(arena, s, depth + 1);
    }
    stripws_level(arena, parent);
    if depth == 0 {
        while arena.nodes[parent].kids.last().is_some_and(|&k| arena.is_ws(k)) {
            arena.nodes[parent].kids.pop();
        }
    }
}

fn stripws_level(arena: &mut Arena, parent: usize) {
    let kind = arena.nodes[parent].kind;
    match kind {
        NKind::Grp(GroupKind::IdentifierList) => {
            // Snapshot ids; remove ws immediately before ',' (issue140).
            let snap = arena.nodes[parent].kids.clone();
            let mut last_nl: Option<usize> = None;
            for &k in &snap {
                let is_comma = arena.tok_match(k, TType::TOKEN_PUNCTUATION, Some(MatchVal::One(",")));
                if last_nl.is_some() && is_comma {
                    let w = last_nl.unwrap();
                    if let Some(pos) = arena.position(parent, w) {
                        arena.remove_at(parent, pos);
                    }
                }
                last_nl = if arena.is_ws(k) { Some(k) } else { None };
            }
            stripws_default(arena, parent);
        }
        NKind::Grp(GroupKind::Parenthesis) => {
            while arena.nodes[parent].kids.get(1).is_some_and(|&k| arena.is_ws(k)) {
                arena.remove_at(parent, 1);
            }
            while arena.nodes[parent].kids.len() >= 2
                && arena.nodes[parent].kids[arena.nodes[parent].kids.len() - 2] != usize::MAX
                && arena.is_ws(arena.nodes[parent].kids[arena.nodes[parent].kids.len() - 2])
            {
                let l = arena.nodes[parent].kids.len() - 2;
                arena.remove_at(parent, l);
            }
            let l = arena.nodes[parent].kids.len();
            if l >= 2 {
                let inner = arena.nodes[parent].kids[l - 2];
                if arena.is_group(inner) {
                    while arena.nodes[inner].kids.last().is_some_and(|&k| arena.is_ws(k)) {
                        arena.nodes[inner].kids.pop();
                    }
                }
            }
            stripws_default(arena, parent);
        }
        _ => stripws_default(arena, parent),
    }
}

fn stripws_default(arena: &mut Arena, parent: usize) {
    let mut last_was_ws = false;
    let mut is_first_char = true;
    let kids = arena.nodes[parent].kids.clone();
    for k in kids {
        if arena.is_ws(k) {
            let v = if last_was_ws || is_first_char { "" } else { " " };
            arena.nodes[k].text = v.to_owned();
        }
        // NOTE: upstream reads `token.is_whitespace` AFTER the assignment
        // for `last_was_ws` (a whitespace token stays whitespace).
        last_was_ws = arena.is_ws(k);
        is_first_char = false;
    }
}

/// Mirrors `SpacesAroundOperatorsFilter` (per level, sublists first).
fn spaces_around_operators(arena: &mut Arena, parent: usize) {
    let subs: Vec<usize> = arena.nodes[parent].kids.clone().into_iter().filter(|&k| arena.is_group(k)).collect();
    for s in subs {
        spaces_around_operators(arena, s);
    }
    let mut t = arena.next_by_either(parent, -1, TType::TOKEN_OPERATOR, TType::TOKEN_OPERATOR_COMPARISON);
    while let Some(mut i) = t {
        // insert_after(tidx): operator stays at i.
        if arena.next(parent, i, false, false).is_some_and(|n| {
            arena.nodes[arena.nodes[parent].kids[n]].ttype != Some(TType::TOKEN_TEXT_WHITESPACE)
        }) {
            let w = ws_leaf(arena, parent, " ");
            arena.insert_after(parent, i, w);
        }
        if arena.prev(parent, i, false, false).is_some_and(|p| {
            arena.nodes[arena.nodes[parent].kids[p]].ttype != Some(TType::TOKEN_TEXT_WHITESPACE)
        }) {
            let w = ws_leaf(arena, parent, " ");
            arena.insert_at(parent, i, w);
            i += 1;
        }
        t = arena.next_by_either(parent, i as isize, TType::TOKEN_OPERATOR, TType::TOKEN_OPERATOR_COMPARISON);
    }
}

/// Python `str.splitlines` boundaries as char counts per line.
fn splitlines_lens(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut cur = 0usize;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\r' {
            if it.peek() == Some(&'\n') {
                it.next();
            }
            out.push(cur);
            cur = 0;
        } else if c == '\n'
            || c == '\x0b'
            || c == '\x0c'
            || ('\x1c'..='\x1e').contains(&c)
            || c == '\u{85}'
            || c == '\u{2028}'
            || c == '\u{2029}'
        {
            out.push(cur);
            cur = 0;
        } else {
            cur += 1;
        }
    }
    out.push(cur);
    out
}

struct Reindent<'a> {
    opts: &'a FormatOptions,
    indent: isize,
    offset: isize,
    root: usize,
    last_render: Option<String>,
    last_func: Option<usize>,
}

impl<'a> Reindent<'a> {
    fn leading(&self) -> isize {
        self.offset + self.indent * self.opts.indent_width as isize
    }

    fn nl(&mut self, arena: &mut Arena, parent: usize, off: isize) -> usize {
        let w = (self.leading() + off).max(0) as usize;
        let mut s = String::from("\n");
        s.extend(std::iter::repeat(self.opts.indent_char).take(w));
        arena.leaf(TType::TOKEN_TEXT_WHITESPACE, s, Some(parent))
    }

    /// Mirrors `_current_line_len` exactly, including its odd tail arms.
    fn current_line_len(&self, arena: &Arena, id: usize) -> usize {
        let mut length = 0usize;
        let mut node = id;
        while node != self.root {
            let parent = match arena.nodes[node].parent {
                Some(p) => p,
                None => break,
            };
            let pos = match arena.position(parent, node) {
                Some(p) => p,
                None => break,
            };
            let mut stack: Vec<usize> = arena.nodes[parent].kids[..pos].to_vec();
            while let Some(prev) = stack.pop() {
                if arena.is_group(prev) {
                    stack.extend(arena.nodes[prev].kids.iter());
                    continue;
                }
                let value = &arena.nodes[prev].text;
                let size = value.chars().count();
                if size == 0 {
                    continue;
                }
                let lines = splitlines_lens(value);
                if lines.len() == 1 && lines[0] == size {
                    length += size;
                    continue;
                }
                let dotted = format!("{value}.");
                let lines = splitlines_lens(&dotted);
                let tail = lines.last().copied().unwrap_or(1).saturating_sub(1) + length;
                if tail != 0 {
                    return tail;
                }
                if lines.len() > 2 {
                    return lines[lines.len() - 2];
                }
                length = lines[0];
            }
            node = parent;
        }
        length
    }

    fn get_offset(&self, arena: &Arena, id: usize) -> isize {
        self.current_line_len(arena, id) as isize - self.leading()
    }

    fn within(&self, arena: &Arena, id: usize, kinds: &[GroupKind]) -> bool {
        let mut n = arena.nodes[id].parent;
        while let Some(p) = n {
            if matches!(arena.nodes[p].kind, NKind::Grp(k) if kinds.contains(&k)) {
                return true;
            }
            n = arena.nodes[p].parent;
        }
        false
    }

    fn get_identifiers(&self, arena: &Arena, parent: usize) -> Vec<usize> {
        arena.nodes[parent]
            .kids
            .iter()
            .copied()
            .filter(|&k| {
                !arena.is_ws(k)
                    && !arena.tok_match(k, TType::TOKEN_PUNCTUATION, Some(MatchVal::One(",")))
            })
            .collect()
    }

    #[allow(clippy::type_complexity)]
    fn get_cases(
        &self, arena: &Arena, parent: usize, skip_ws: bool,
    ) -> Vec<(Option<Vec<usize>>, Vec<usize>)> {
        let mut ret: Vec<(Option<Vec<usize>>, Vec<usize>)> = Vec::new();
        // Upstream starts in CONDITION mode (leading tokens before the
        // first WHEN open the first pair).
        let mut mode: Option<bool> = Some(true); // true = condition
        for &k in &arena.nodes[parent].kids.clone() {
            if arena.tok_match(k, TType::TOKEN_KEYWORD, Some(MatchVal::One("CASE")))
                || (skip_ws
                    && matches!(arena.nodes[k].ttype, Some(t) if t.is_in(TType::TOKEN_TEXT_WHITESPACE)))
            {
                continue;
            } else if arena.tok_match(k, TType::TOKEN_KEYWORD, Some(MatchVal::One("WHEN"))) {
                ret.push((Some(Vec::new()), Vec::new()));
                mode = Some(true);
            } else if arena.tok_match(k, TType::TOKEN_KEYWORD, Some(MatchVal::One("THEN"))) {
                mode = Some(false);
            } else if arena.tok_match(k, TType::TOKEN_KEYWORD, Some(MatchVal::One("ELSE"))) {
                ret.push((None, Vec::new()));
                mode = Some(false);
            } else if arena.tok_match(k, TType::TOKEN_KEYWORD, Some(MatchVal::One("END"))) {
                mode = None;
            }
            if mode.is_some() && ret.is_empty() {
                ret.push((Some(Vec::new()), Vec::new()));
            }
            match mode {
                Some(true) => match ret.last_mut() {
                    Some((Some(c), _)) => c.push(k),
                    _ => ret.push((Some(vec![k]), Vec::new())),
                },
                Some(false) => match ret.last_mut() {
                    Some((_, v)) => v.push(k),
                    None => ret.push((Some(Vec::new()), vec![k])),
                },
                None => {}
            }
        }
        ret
    }

    fn first_leaf(&self, arena: &Arena, id: usize) -> Option<usize> {
        let mut n = id;
        loop {
            if !arena.is_group(n) {
                return Some(n);
            }
            n = *arena.nodes[n].kids.first()?;
        }
    }

    fn next_token(
        &self, arena: &Arena, parent: usize, idx: isize,
    ) -> (Option<usize>, Option<usize>) {
        let split = [
            "FROM", "JOIN$", "STRAIGHT_JOIN$", "AND", "OR", "GROUP BY", "ORDER BY", "UNION", "VALUES",
            "SET", "BETWEEN", "EXCEPT", "HAVING", "LIMIT",
        ];
        let mut r = self.next_by_words(arena, parent, &split, idx);
        if r.is_some_and(|i| {
            let k = arena.nodes[parent].kids[i];
            arena.norm_eq(k, "BETWEEN")
        }) {
            let i = r.unwrap();
            let (r2, _) = self.next_token(arena, parent, i as isize);
            r = r2;
            if r.is_some_and(|j| {
                let k = arena.nodes[parent].kids[j];
                arena.norm_eq(k, "AND")
            }) {
                let j = r.unwrap();
                let (r3, _) = self.next_token(arena, parent, j as isize);
                r = r3;
            }
        }
        match r {
            Some(i) => (Some(i), Some(arena.nodes[parent].kids[i])),
            None => (None, None),
        }
    }

    fn next_by_words(
        &self, arena: &Arena, parent: usize, words: &[&str], idx: isize,
    ) -> Option<usize> {
        let kids = &arena.nodes[parent].kids;
        let mut i = (idx + 1).max(0) as usize;
        while i < kids.len() {
            let k = kids[i];
            // Upstream `match` uses `is` (exact): DML/DDL/Order never match here.
            if matches!(arena.nodes[k].ttype, Some(t) if t == TType::TOKEN_KEYWORD)
                && words.iter().any(|w| {
                    // All 14 entries compile as regexes and `search` (regex=True):
                    // plain words match anywhere, `$` words anchor at the end.
                    if let Some(p) = w.strip_suffix('$') {
                        arena.nodes[k].text.to_uppercase().ends_with(p)
                    } else {
                        arena.nodes[k].text.to_uppercase().contains(w)
                    }
                })
            {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn split_kwds(&mut self, arena: &mut Arena, parent: usize) {
        let (mut t, _) = self.next_token(arena, parent, -1);
        while let Some(i) = t {
            if let Some(p) = arena.prev(parent, i, false, false) {
                let pid = arena.nodes[parent].kids[p];
                let uprev = arena.group_text_live(pid);
                if arena.is_ws(pid) {
                    arena.remove_at(parent, p);
                    // Token shifted left to ni; insert before it unless the
                    // removed whitespace already ended the line.
                    let ni = i - 1;
                    if !(uprev.ends_with('\n') || uprev.ends_with('\r')) {
                        let n = self.nl(arena, parent, 0);
                        arena.insert_at(parent, ni, n);
                        // Token now at ni + 1; continue after it.
                        let (nt, _) = self.next_token(arena, parent, (ni + 1) as isize);
                        t = nt;
                    } else {
                        let (nt, _) = self.next_token(arena, parent, ni as isize);
                        t = nt;
                    }
                    continue;
                }
                if !(uprev.ends_with('\n') || uprev.ends_with('\r')) {
                    let n = self.nl(arena, parent, 0);
                    arena.insert_at(parent, i, n);
                    // Token now at i + 1; continue after it.
                    let (nt, _) = self.next_token(arena, parent, (i + 1) as isize);
                    t = nt;
                    continue;
                }
            } else {
                // prev is None: uprev is str(None) == 'None', never newline-y.
                let n = self.nl(arena, parent, 0);
                arena.insert_at(parent, i, n);
                let (nt, _) = self.next_token(arena, parent, (i + 1) as isize);
                t = nt;
                continue;
            }
            let (nt, _) = self.next_token(arena, parent, (i + 1) as isize);
            t = nt;
        }
    }
    fn split_statements(&mut self, arena: &mut Arena, parent: usize) {
        let mut t = self.next_dml(arena, parent, -1);
        while let Some(i) = t {
            if let Some(p) = arena.prev(parent, i, false, false) {
                let pid = arena.nodes[parent].kids[p];
                if arena.is_ws(pid) {
                    arena.remove_at(parent, p);
                    let ni = i - 1;
                    // only break if it's not the first token
                    let n = self.nl(arena, parent, 0);
                    arena.insert_at(parent, ni, n);
                    // Token back at i; continue after it.
                    t = self.next_dml(arena, parent, i as isize);
                    continue;
                }
                // prev_ exists (index could be 0): insert break.
                let n = self.nl(arena, parent, 0);
                arena.insert_at(parent, i, n);
                // Token now at i + 1; continue after it.
                t = self.next_dml(arena, parent, (i + 1) as isize);
                continue;
            }
            // No prev (first token): no break.
            t = self.next_dml(arena, parent, i as isize);
        }
    }

    fn next_dml(&self, arena: &Arena, parent: usize, idx: isize) -> Option<usize> {
        let kids = &arena.nodes[parent].kids;
        let mut i = (idx + 1).max(0) as usize;
        while i < kids.len() {
            if matches!(arena.nodes[kids[i]].ttype, Some(t) if t == TType::TOKEN_KEYWORD_DML || t == TType::TOKEN_KEYWORD_DDL)
            {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn process_node(&mut self, arena: &mut Arena, parent: usize, id: usize) {
        match arena.nodes[id].kind {
            NKind::Grp(GroupKind::IdentifierList) => self.process_identifierlist(arena, id),
            NKind::Grp(GroupKind::Parenthesis) => self.process_parenthesis(arena, id),
            NKind::Grp(GroupKind::Function) => {
                if let Some(&f) = arena.nodes[id].kids.first() {
                    self.last_func = Some(f);
                }
                self.process_default(arena, id, true);
            }
            NKind::Grp(GroupKind::Where) => self.process_where(arena, id),
            NKind::Grp(GroupKind::Case) => self.process_case(arena, id),
            NKind::Grp(GroupKind::Values) => self.process_values(arena, id),
            _ => self.process_default(arena, id, true),
        }
        let _ = parent;
    }

    fn process_where(&mut self, arena: &mut Arena, id: usize) {
        let t = arena.next_by(id, -1, &[], Some((TType::TOKEN_KEYWORD, MatchVal::One("WHERE"))), None);
        if t.is_none() {
            return;
        }
        let n = self.nl(arena, id, 0);
        arena.insert_at(id, t.unwrap(), n);
        let sv = self.indent;
        self.indent += 1;
        self.process_default(arena, id, true);
        self.indent = sv;
    }

    fn process_parenthesis(&mut self, arena: &mut Arena, id: usize) {
        let is_dml = arena.next_by_either(id, -1, TType::TOKEN_KEYWORD_DML, TType::TOKEN_KEYWORD_DDL).is_some();
        let f = arena.next_by(id, -1, &[], Some((TType::TOKEN_PUNCTUATION, MatchVal::One("("))), None);
        let first = match f {
            Some(x) => x,
            None => return,
        };
        let sv = self.indent;
        if is_dml {
            self.indent += 1;
        }
        // Capture the node id: the insert below shifts indices.
        let first_id = arena.nodes[id].kids[first];
        if is_dml {
            let n = self.nl(arena, id, 0);
            arena.insert_at(id, 0, n);
        }
        let off = self.get_offset(arena, first_id) + 1;
        let so = self.offset;
        self.offset += off;
        self.process_default(arena, id, !is_dml);
        self.offset = so;
        self.indent = sv;
    }

    fn process_identifierlist(&mut self, arena: &mut Arena, id: usize) {
        let mut identifiers = self.get_identifiers(arena, id);
        let num_offset: isize = if self.opts.indent_columns {
            let first_leaf = identifiers.first().and_then(|&x| self.first_leaf(arena, x));
            let _ = first_leaf;
            if self.opts.indent_char == '\t' { 1 } else { self.opts.indent_width as isize }
        } else {
            let first_id = identifiers.remove(0);
            let fl = self.first_leaf(arena, first_id);
            if self.opts.indent_char == '\t' {
                1
            } else {
                fl.map(|f| self.get_offset(arena, f)).unwrap_or(0)
            }
        };
        if !self.within(arena, id, &[GroupKind::Function, GroupKind::Values]) {
            let so = self.offset;
            self.offset += num_offset;
            let mut position = 0usize;
            for &tok in &identifiers.clone() {
                position += arena.group_value(tok).chars().count() + 1;
                if position as isize > self.opts.wrap_after as isize - self.offset {
                    let mut adjust = 0isize;
                    let mut target = tok;
                    if self.opts.comma_first {
                        adjust = -2;
                        let pos = arena.position(id, tok).unwrap();
                        // Previous non-whitespace is the comma itself.
                        match arena.prev(id, pos, true, false) {
                            None => continue,
                            Some(ci) => target = arena.nodes[id].kids[ci],
                        }
                    }
                    let n = self.nl(arena, id, adjust);
                    let pos = arena.position(id, target).unwrap();
                    arena.insert_at(id, pos, n);
                    if self.opts.comma_first {
                        let ti = arena.position(id, target).unwrap();
                        if let Some(w) = arena.next(id, ti, false, false) {
                            let wid = arena.nodes[id].kids[w];
                            if arena.nodes[wid].ttype != Some(TType::TOKEN_TEXT_WHITESPACE) {
                                let s = ws_leaf(arena, id, " ");
                                arena.insert_at(id, w, s);
                            }
                        }
                    }
                    position = 0;
                }
            }
            self.offset = so;
        } else {
            for &tok in &arena.nodes[id].kids.clone() {
                if arena.group_value(tok) == "," {
                    if arena.next(id, arena.position(id, tok).unwrap(), false, false).is_some_and(|n| {
                        !arena.is_ws(arena.nodes[id].kids[n])
                    }) {
                        let s = ws_leaf(arena, id, " ");
                        let pos = arena.position(id, tok).unwrap();
                        arena.insert_at(id, pos + 1, s);
                    }
                }
            }
            let end_at = self.offset
                + identifiers.iter().map(|&x| arena.group_value(x).chars().count() as isize + 1).sum::<isize>();
            let mut adjusted = 0isize;
            if self.opts.wrap_after > 0
                && end_at > self.opts.wrap_after as isize - self.offset
                && self.last_func.is_some()
            {
                let lf = self.last_func.unwrap();
                adjusted = -(arena.group_value(lf).chars().count() as isize) - 1;
            }
            let (sv_i, sv_o) = (self.indent, self.offset);
            self.offset += adjusted;
            self.indent += 1;
            if adjusted < 0 {
                let n = self.nl(arena, id, 0);
                let pos = arena.position(id, identifiers[0]).unwrap();
                arena.insert_at(id, pos, n);
            }
            let mut position = 0usize;
            for &tok in &identifiers.clone() {
                position += arena.group_value(tok).chars().count() + 1;
                if self.opts.wrap_after > 0
                    && position as isize > self.opts.wrap_after as isize - self.offset
                {
                    let n = self.nl(arena, id, 0);
                    let pos = arena.position(id, tok).unwrap();
                    arena.insert_at(id, pos, n);
                    position = 0;
                }
            }
            self.indent = sv_i;
            self.offset = sv_o;
        }
        self.process_default(arena, id, true);
    }

    fn process_case(&mut self, arena: &mut Arena, id: usize) {
        let cases = self.get_cases(arena, id, false);
        let first_pair = cases.first();
        let first_cond_empty = matches!(first_pair, Some((None, _)) | None);
        let first: Option<usize> = match first_pair {
            Some((Some(c), _)) if !c.is_empty() => self.first_leaf(arena, c[0]),
            // Upstream indexes cond[0] unconditionally (TypeError on ELSE-
            // first or empty WHEN); fall back to the group's first leaf.
            _ if first_cond_empty => self.first_leaf(arena, id),
            _ => None,
        };
        let kids0 = arena.nodes[id].kids.first().copied();
        let so1 = self.offset;
        self.offset += kids0.map(|k| self.get_offset(arena, k)).unwrap_or(0);
        let so2 = self.offset;
        self.offset += first.map(|f| self.get_offset(arena, f)).unwrap_or(0);
        for (cond, value) in cases.iter().skip(1) {
            let str_cond: String = cond.as_ref().map(|c| c.iter().map(|&x| arena.group_text_live(x)).collect()).unwrap_or_default();
            let str_value: String = value.iter().map(|&x| arena.group_text_live(x)).collect();
            let end_pos = self.offset + 1 + str_cond.chars().count() as isize + str_value.chars().count() as isize;
            if !self.opts.compact && end_pos > self.opts.wrap_after as isize {
                let target = match cond {
                    Some(c) if !c.is_empty() => c[0],
                    _ => match value.first() {
                        Some(&v) => v,
                        None => continue,
                    },
                };
                // Upstream uses cond[0] / value[0] unconditionally (crash on
                // empties); skip unrenderable arms instead.
                let n = self.nl(arena, id, 0);
                let pos = arena.position(id, target).unwrap();
                arena.insert_at(id, pos, n);
            }
        }
        self.offset += "WHEN ".len() as isize;
        self.process_default(arena, id, true);
        self.offset = so2;
        let e = arena.next_by(id, -1, &[], Some((TType::TOKEN_KEYWORD, MatchVal::One("END"))), None);
        if e.is_some() && !self.opts.compact {
            let n = self.nl(arena, id, 0);
            arena.insert_at(id, e.unwrap(), n);
        }
        self.offset = so1;
    }

    fn process_values(&mut self, arena: &mut Arena, id: usize) {
        let n = self.nl(arena, id, 0);
        arena.insert_at(id, 0, n);
        let mut t = arena.next_by(id, -1, &[GroupKind::Parenthesis], None, None);
        while let Some(i) = t {
            let first_token = arena.nodes[id].kids[i];
            let cur = arena.next_by(
                id, i as isize, &[],
                Some((TType::TOKEN_PUNCTUATION, MatchVal::One(","))), None,
            );
            // Upstream loops while token: only the FIRST comma after each
            // parenthesis is consumed per iteration (single if, not while).
            if let Some(p) = cur {
                if self.opts.comma_first {
                    let adjust = -2isize;
                    let off = self.get_offset(arena, first_token) + adjust;
                    let nn = self.nl(arena, id, off);
                    arena.insert_at(id, p, nn);
                } else {
                    let off = self.get_offset(arena, arena.nodes[id].kids[i]);
                    let nn = self.nl(arena, id, off);
                    arena.insert_after(id, p, nn);
                }
                let _ = cur;
            }
            t = arena.next_by(id, i as isize, &[GroupKind::Parenthesis], None, None);
        }
    }

    fn process_default(&mut self, arena: &mut Arena, id: usize, stmts: bool) {
        if stmts {
            self.split_statements(arena, id);
        }
        self.split_kwds(arena, id);
        let subs: Vec<usize> = arena.nodes[id].kids.clone().into_iter().filter(|&k| arena.is_group(k)).collect();
        for s in subs {
            self.process_node(arena, id, s);
        }
    }

    fn process(&mut self, arena: &mut Arena, root: usize) -> String {
        self.root = root;
        self.process_node(arena, root, root);
        if let Some(prev) = self.last_render.take() {
            let sep = if prev.ends_with('\n') { "\n" } else { "\n\n" };
            let w = ws_leaf(arena, root, sep);
            arena.insert_at(root, 0, w);
        }
        let out = arena.group_text_live(root);
        self.last_render = Some(out.clone());
        out
    }
}

/// Mirrors `AlignedIndentFilter` (operates on the same arena after reindent).
struct Aligned<'a> {
    opts: &'a FormatOptions,
    indent: isize,
    offset: isize,
}

impl<'a> Aligned<'a> {
    fn nl(&self, arena: &mut Arena, parent: usize, off: AlignedOff) -> usize {
        let o = match off {
            AlignedOff::Int(i) => i,
            AlignedOff::Str(s) => -(s.chars().count() as isize),
        };
        let w = (6 + o + self.indent * (2 + 6) + self.offset).max(0) as usize;
        let mut s = String::from("\n");
        s.extend(std::iter::repeat(self.opts.indent_char).take(w));
        arena.leaf(TType::TOKEN_TEXT_WHITESPACE, s, Some(parent))
    }

    fn match_join(s: &str) -> bool {
        let w: Vec<&str> = s.split_whitespace().collect();
        if w.last() != Some(&"JOIN") {
            return false;
        }
        match &w[..w.len() - 1] {
            [] => true,
            [a] => ["LEFT", "RIGHT", "FULL", "INNER", "OUTER", "STRAIGHT", "CROSS", "NATURAL"].contains(a),
            [a, b] => {
                ["LEFT", "RIGHT", "FULL"].contains(a) && ["INNER", "OUTER", "STRAIGHT"].contains(b)
            }
            _ => false,
        }
    }

    fn match_by(s: &str) -> bool {
        let w: Vec<&str> = s.split_whitespace().collect();
        w == ["GROUP", "BY"] || w == ["ORDER", "BY"]
    }

    fn next_token(&self, arena: &Arena, parent: usize, idx: isize) -> Option<usize> {
        let kids = &arena.nodes[parent].kids;
        let mut i = (idx + 1).max(0) as usize;
        while i < kids.len() {
            let k = kids[i];
            // Exact Keyword only (upstream `match` identity).
            if matches!(arena.nodes[k].ttype, Some(t) if t == TType::TOKEN_KEYWORD) {
                let up = arena.nodes[k].text.to_uppercase();
                // All 14 split words compile as regexes and `search`
                // (regex=True): plain words match anywhere.
                if ["FROM", "ON", "WHERE", "AND", "OR", "HAVING", "LIMIT",
                    "UNION", "VALUES", "SET", "BETWEEN", "EXCEPT"]
                    .iter()
                    .any(|w| up.contains(w))
                    || Self::match_join(&up)
                    || Self::match_by(&up)
                {
                    let mut r = Some(i);
                    if arena.norm_eq(k, "BETWEEN") {
                        r = self.next_token(arena, parent, i as isize);
                        if r.is_some_and(|j| arena.norm_eq(arena.nodes[parent].kids[j], "AND")) {
                            let j = r.unwrap();
                            r = self.next_token(arena, parent, j as isize);
                        }
                    }
                    return r;
                }
            }
            i += 1;
        }
        None
    }

    fn process_node(&mut self, arena: &mut Arena, parent: usize, id: usize) {
        match arena.nodes[id].kind {
            NKind::Grp(GroupKind::Statement) => self.process_statement(arena, id),
            NKind::Grp(GroupKind::Parenthesis) => self.process_parenthesis(arena, id),
            NKind::Grp(GroupKind::IdentifierList) => self.process_identifierlist(arena, id),
            NKind::Grp(GroupKind::Case) => self.process_case(arena, id),
            _ => self.process_default(arena, id),
        }
        let _ = parent;
    }

    fn process_statement(&mut self, arena: &mut Arena, id: usize) {
        if self.indent == 0
            && arena.nodes[id].kids.first().is_some_and(|&k| arena.is_ws(k))
        {
            arena.remove_at(id, 0);
        }
        self.process_default(arena, id);
    }

    fn process_parenthesis(&mut self, arena: &mut Arena, id: usize) {
        let found = arena.next_by(id, -1, &[], Some((TType::TOKEN_KEYWORD_DML, MatchVal::One("SELECT"))), None);
        if found.is_none() {
            return;
        }
        self.indent += 1;
        let s = self.nl(arena, id, AlignedOff::Str("SELECT"));
        arena.insert_at(id, 1, s);
        self.process_default(arena, id);
        self.indent -= 1;
        let n = self.nl(arena, id, AlignedOff::Int(1));
        let l = arena.nodes[id].kids.len();
        arena.insert_at(id, l - 1, n);
    }

    fn process_identifierlist(&mut self, arena: &mut Arena, id: usize) {
        let ids: Vec<usize> = arena.nodes[id].kids.iter().copied()
            .filter(|&k| !arena.is_ws(k) && !arena.tok_match(k, TType::TOKEN_PUNCTUATION, Some(MatchVal::One(",")))).collect();
        for &tok in ids.iter().skip(1) {
            // Upstream `nl()` defaults to offset=1 (one space after SELECT).
            let n = self.nl(arena, id, AlignedOff::Int(1));
            let pos = arena.position(id, tok).unwrap();
            arena.insert_at(id, pos, n);
        }
        self.process_default(arena, id);
    }

    fn process_case(&mut self, arena: &mut Arena, id: usize) {
        // get_cases(skip_ws=True) via a temporary Reindent-shaped walk.
        let re = Reindent {
            opts: self.opts, indent: 0, offset: 0,
            root: id, last_render: None, last_func: None,
        };
        let mut cases = re.get_cases(arena, id, true);
        let end = arena.next_by(id, -1, &[], Some((TType::TOKEN_KEYWORD, MatchVal::One("END"))), None);
        if let Some(e) = end {
            cases.push((None, vec![arena.nodes[id].kids[e]]));
        }
        let widths: Vec<usize> = cases.iter().map(|(c, _)| {
            c.as_ref().map(|cc| {
                cc.iter().map(|&x| arena.group_text_live(x)).collect::<Vec<_>>().join(" ").chars().count()
            }).unwrap_or(0)
        }).collect();
        let maxw = widths.iter().copied().max().unwrap_or(0);
        for (i, (cond, value)) in cases.iter().enumerate() {
            let stmt = match cond {
                Some(c) if !c.is_empty() => Some(c[0]),
                _ => value.first().copied(),
            };
            let stmt = match stmt {
                Some(s) => s,
                None => continue,
            };
            if i > 0 {
                let n = self.nl(arena, id, AlignedOff::Int(10 - arena.group_text_live(stmt).chars().count() as isize));
                let pos = arena.position(id, stmt).unwrap();
                arena.insert_at(id, pos, n);
            }
            if let Some(c) = cond {
                if !c.is_empty() {
                    let pad = " ".repeat(maxw - widths[i]);
                    let w = ws_leaf(arena, id, &pad);
                    let last = *c.last().unwrap();
                    let pos = arena.position(id, last).unwrap();
                    arena.insert_at(id, pos + 1, w);
                }
            }
        }
    }

    fn split_kwds(&mut self, arena: &mut Arena, id: usize) {
        let mut t = self.next_token(arena, id, -1);
        while let Some(i) = t {
            let tok = arena.nodes[id].kids[i];
            let up = arena.nodes[tok].text.to_uppercase();
            let token_indent = if Self::match_join(&up) || Self::match_by(&up) {
                up.split_whitespace().next().unwrap_or("").to_owned()
            } else {
                arena.group_text_live(tok)
            };
            let n = self.nl(arena, id, AlignedOff::Str(&token_indent));
            arena.insert_at(id, i, n);
            t = self.next_token(arena, id, (i + 1) as isize);
        }
    }

    fn process_default(&mut self, arena: &mut Arena, id: usize) {
        self.split_kwds(arena, id);
        let subs: Vec<usize> = arena.nodes[id].kids.clone().into_iter().filter(|&k| arena.is_group(k)).collect();
        for s in subs {
            let pos = arena.position(id, s).unwrap();
            let prev = if pos > 0 {
                let mut j = pos;
                let mut found = None;
                while j > 0 {
                    j -= 1;
                    let k = arena.nodes[id].kids[j];
                    if !arena.is_ws(k) {
                        found = Some(k);
                        break;
                    }
                }
                found
            } else {
                None
            };
            let off = if prev.is_some_and(|p| {
                matches!(arena.nodes[p].ttype, Some(t) if t.is_in(TType::TOKEN_KEYWORD))
                    && Self::match_by(&arena.nodes[p].text.to_uppercase())
            }) {
                3
            } else {
                0
            };
            let so = self.offset;
            self.offset += off;
            self.process_node(arena, id, s);
            self.offset = so;
        }
    }
}

enum AlignedOff<'a> {
    Int(isize),
    Str(&'a str),
}

/// Mirrors `OutputPythonFilter` / `OutputPHPFilter` + `format()` wiring.
/// Operates on the top-level kids; groups render wholesale (escaped as one
/// unit, like upstream's `token.value.replace(...)` on the group).
fn output_wrap(arena: &Arena, root: usize, rendered: &str, output: u8, varname: &str, count: usize) -> String {
    if output == 0 {
        return rendered.to_owned();
    }
    // `str.strip().splitlines()` — universal boundaries, not `lines()`.
    let has_nl = splitlines_lens(rendered.trim()).len() > 1;
    let mut out = String::new();
    let push = |o: &mut String, s: &str| o.push_str(s);
    if output == 1 {
        // Python.
        if count > 1 {
            push(&mut out, "\n");
        }
        push(&mut out, varname);
        push(&mut out, " = ");
        if has_nl {
            push(&mut out, "(");
        }
        push(&mut out, "'");
        for &k in &arena.nodes[root].kids.clone() {
            if !arena.is_group(k) && arena.is_ws(k) && arena.nodes[k].text.contains('\n') {
                push(&mut out, " '");
                push(&mut out, "\n");
                push(&mut out, &" ".repeat(varname.chars().count() + 4));
                push(&mut out, "'");
                if let Some(after) = arena.nodes[k].text.split_once('\n') {
                    push(&mut out, after.1);
                }
                continue;
            }
            let raw = arena.group_value(k);
            push(&mut out, &raw.replace('\\', "\\\\").replace('\'', "\\'"));
        }
        push(&mut out, "'");
        if has_nl {
            push(&mut out, ")");
        }
    } else {
        // PHP.
        if count > 1 {
            push(&mut out, "\n");
        }
        push(&mut out, varname);
        push(&mut out, " ");
        if has_nl {
            push(&mut out, " ");
        }
        push(&mut out, "= ");
        push(&mut out, "\"");
        for &k in &arena.nodes[root].kids.clone() {
            if !arena.is_group(k) && arena.is_ws(k) && arena.nodes[k].text.contains('\n') {
                push(&mut out, " \";");
                push(&mut out, "\n");
                push(&mut out, varname);
                push(&mut out, " ");
                push(&mut out, ".= ");
                push(&mut out, "\"");
                if let Some(after) = arena.nodes[k].text.split_once('\n') {
                    push(&mut out, after.1);
                }
                continue;
            }
            let raw = arena.group_value(k);
            push(&mut out, &raw.replace('\\', "\\\\").replace('"', "\\\""));
        }
        push(&mut out, "\";");
    }
    out
}

/// Mirrors `split_unquoted_newlines`: split on newlines outside quotes.
fn split_unquoted(s: &str) -> Vec<String> {
    let mut lines = vec![String::new()];
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' {
            // Look for the closing quote (backslash escapes honored).
            let mut j = i + 1;
            let mut closed = false;
            while j < chars.len() {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if chars[j] == c {
                    closed = true;
                    break;
                }
                j += 1;
            }
            if closed {
                for k in i..=j {
                    lines.last_mut().unwrap().push(chars[k]);
                }
                i = j + 1;
            } else {
                // Unterminated: the quote is a lone separator chunk.
                lines.last_mut().unwrap().push(c);
                i += 1;
            }
            continue;
        }
        if c == '\r' {
            if chars.get(i + 1) == Some(&'\n') {
                i += 1;
            }
            lines.push(String::new());
        } else if c == '\n' {
            lines.push(String::new());
        } else {
            lines.last_mut().unwrap().push(c);
        }
        i += 1;
    }
    lines
}

/// One statement through the full filter stack (mirrors `format()` +
/// `FilterStack.run` for a single grouped statement).
fn format_one(
    text: &str,
    kids: &[tree::Node],
    opts: &FormatOptions,
    state: &mut ReindentState,
) -> Result<String, FormatError> {
    let mut arena = Arena::new();
    let root = arena.group(GroupKind::Statement, None);
    let top = arena.from_tree(text, kids, Some(root));
    arena.nodes[root].kids = top;
    if opts.space_ops {
        spaces_around_operators(&mut arena, root);
    }
    if opts.strip_comments {
        strip_comments(&mut arena, root);
    }
    if opts.strip_ws {
        strip_whitespace(&mut arena, root, 0);
    }
    if opts.reindent {
        let mut r = Reindent {
            opts, indent: if opts.indent_after_first { 1 } else { 0 },
            offset: 0, root, last_render: state.last_render.take(), last_func: None,
        };
        r.process(&mut arena, root);
        state.last_render = r.last_render;
    }
    if opts.reindent_aligned {
        let mut a = Aligned { opts, indent: 0, offset: 0 };
        a.process_node(&mut arena, root, root);
    }
    // RightMarginFilter is wired (when set) but raises on first use.
    if opts.right_margin.is_some() {
        return Err(FormatError::NotImplemented);
    }
    let rendered = arena.group_text_live(root);
    // Upstream `OutputFilter._process` rebinds `stmt.tokens` to a one-shot
    // generator that `SerializerUnicode` exhausts via `str()`: with output
    // wrap, `_last_stmt` renders as `''` for every later statement (hence
    // `\n\n`). Without wrap the tree stays intact (live render).
    state.last_render = Some(if opts.output == 0 {
        rendered.clone()
    } else {
        String::new()
    });
    let wrapped = if opts.output == 0 {
        rendered
    } else {
        state.count += 1;
        // `varname` / `varname{count}` exactly like `OutputFilter.process`.
        let base = if opts.output == 2 { "$sql" } else { "sql" };
        let vn = if state.count > 1 {
            format!("{base}{}", state.count)
        } else {
            base.to_owned()
        };
        output_wrap(&arena, root, &rendered, opts.output, &vn, state.count)
    };
    // SerializerUnicode: rstrip each line, join with \n.
    let lines = split_unquoted(&wrapped);
    Ok(lines.iter().map(|l| l.trim_end_matches(|c: char| c.is_whitespace()).to_owned()).collect::<Vec<_>>().join("\n"))
}

struct ReindentState {
    last_render: Option<String>,
    count: usize,
}

/// Format entry: preprocess → split → group → stmt filters → serialize.
/// `grouping` mirrors `build_filter_stack`'s `enable_grouping` calls.
pub fn format_text(text: &str, opts: &FormatOptions) -> Result<String, FormatError> {
    let grouping = opts.space_ops
        || opts.strip_comments
        || opts.strip_ws
        || opts.reindent
        || opts.reindent_aligned
        || opts.right_margin.is_some();
    let (text2, toks2) = preprocess(text, &crate::lexer::lex(text), opts);
    let mut splitter = crate::split::Splitter::new();
    let stmts = splitter.split(&text2, &toks2);
    let mut state = ReindentState { last_render: None, count: 0 };
    let mut out = String::new();
    for stmt in &stmts {
        let flat: Vec<tree::Node> = stmt.iter().map(|t| tree::Node::leaf(t.ttype, t)).collect();
        let mut kids = flat;
        if grouping {
            crate::group::group_stmt(&text2, &mut kids).map_err(FormatError::Group)?;
        }
        out.push_str(&format_one(&text2, &kids, opts, &mut state)?);
    }
    Ok(out)
}

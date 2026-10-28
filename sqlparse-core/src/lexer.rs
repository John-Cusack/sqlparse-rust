//! Default-configuration lexer for sqlparse 0.6.0.
//!
//! Hand-written char scanner implementing `SQL_REGEX` (50 patterns, in
//! order) + `find_delimited_spans` + default keyword maps. Custom lexer
//! configuration (`clear`/`set_SQL_REGEX`/`add_keywords`) stays Python
//! behind the facade seam; this module only implements
//! `default_initialization` behavior, verified token-for-token against
//! CPython by the differential fuzz corpus.
//!
//! Char-space matching throughout (CPython `re.match(string, pos)`
//! semantics); spans cross to the binding as byte offsets.

use core::cmp::Ordering;

use crate::tables_gen::{KEYWORDS, ND_RANGES, TType, W_RANGES};

/// One lexed token: type + byte span of its value in the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawTok {
    pub ttype: TType,
    pub start: u32,
    pub end: u32,
}

/// Binary search over closed ranges.
#[inline]
fn in_ranges(ranges: &[(u32, u32)], c: char) -> bool {
    let cp = c as u32;
    ranges
        .binary_search_by(|&(s, e)| {
            if cp < s {
                Ordering::Greater
            } else if cp > e {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// Python `str` `\w` = L* + N* + `_` exactly (generated `W_RANGES`;
/// `char::is_alphanumeric` over-covers `Other_Alphabetic` marks).
#[inline]
fn is_word(c: char) -> bool {
    in_ranges(W_RANGES, c)
}
/// Python `str` `\s` = Unicode `White_Space` = `char::is_whitespace`.
#[inline]
fn is_ws(c: char) -> bool {
    c.is_whitespace()
}

/// Python `str` `\d` = General_Category `Nd` exactly (generated `ND_RANGES`;
/// Rust `to_digit` is ASCII-only).
#[inline]
fn is_digit(c: char) -> bool {
    in_ranges(ND_RANGES, c)
}

/// First char of full lowercase ≈ SRE single-char `TOLOWER`
/// (only U+0130 lowercases multi-char; its head equals the simple fold).
#[inline]
fn fold_lo(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Full uppercase when it is a single char (SRE `TOUPPER`; multi-char
/// results like `ß`→`SS` never participate in a match).
#[inline]
fn up1(c: char) -> Option<char> {
    let mut it = c.to_uppercase();
    let a = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(a)
}

/// SRE `IGNORECASE` literal comparison: equal, ortolower-equal, or
/// single-char-toupper-equal (verified: `ſ`/`ı`/`İ` match, `ß`/`Σ` don't).
#[inline]
fn eq_ci(p: char, t: char) -> bool {
    if p == t {
        return true;
    }
    if fold_lo(p) == fold_lo(t) {
        return true;
    }
    matches!((up1(p), up1(t)), (Some(a), Some(b)) if a == b)
}

/// `[A-ZÀ-Ü]` under `IGNORECASE`: member, folded-lower in `[a-zà-ü]`,
/// or single-char-upper in the sets (covers Kelvin `K`, `ſ`, `İ`, `ı`;
/// excludes `ß`, `Σ`, `Þ`, `ÿ` — all verified against CPython).
#[inline]
fn in_az(c: char) -> bool {
    matches!(c, 'A'..='Z' | 'À'..='Ü')
        || matches!(fold_lo(c), 'a'..='z' | 'à'..='ü')
        || up1(c).is_some_and(|u| matches!(u, 'A'..='Z' | 'À'..='Ü'))
}

/// `[_A-ZÀ-Ü]` under `IGNORECASE` (tags, number guards).
#[inline]
fn in_neg_class(c: char) -> bool {
    c == '_' || in_az(c)
}

/// `\b` at `pos`: exactly one side is a word char.
#[inline]
fn boundary(ch: &[char], pos: usize) -> bool {
    let left = pos.checked_sub(1).is_some_and(|i| is_word(ch[i]));
    let right = ch.get(pos).is_some_and(|&c| is_word(c));
    left != right
}

/// Match literal `word` case-insensitively at `pos`; return end on success.
fn lit(ch: &[char], pos: usize, word: &str) -> Option<usize> {
    let mut p = pos;
    for pc in word.chars() {
        if p >= ch.len() || !eq_ci(pc, ch[p]) {
            return None;
        }
        p += 1;
    }
    Some(p)
}

/// Match one-or-more whitespace; return end.
fn ws1(ch: &[char], pos: usize) -> Option<usize> {
    if pos >= ch.len() || !is_ws(ch[pos]) {
        return None;
    }
    let mut p = pos + 1;
    while p < ch.len() && is_ws(ch[p]) {
        p += 1;
    }
    Some(p)
}

/// Match a `\w+` run; return end.
fn word_run(ch: &[char], mut p: usize) -> usize {
    while p < ch.len() && is_word(ch[p]) {
        p += 1;
    }
    p
}

/// Default keyword lookup: uppercase (full Unicode, like `str.upper`)
/// then first-dict-wins merged map (bake order = upstream init order).
fn is_keyword(value: &str) -> Option<TType> {
    let upper: String = value.chars().flat_map(|c| c.to_uppercase()).collect();
    KEYWORDS
        .binary_search_by(|&(k, _)| k.as_bytes().cmp(upper.as_bytes()))
        .ok()
        .map(|i| KEYWORDS[i].1)
}

/// Run of `\d` from `pos`; returns end.
fn digit_run(ch: &[char], mut p: usize) -> usize {
    while p < ch.len() && is_digit(ch[p]) {
        p += 1;
    }
    p
}

/// `'...'`/`"..."` with doubled-quote and backslash escapes; unterminated
/// (or lone opener) is no match. Mirrors `(''|\\'|[^'])*` shape.
fn match_quoted(ch: &[char], pos: usize, q: char) -> Option<usize> {
    if pos >= ch.len() || ch[pos] != q {
        return None;
    }
    let mut p = pos + 1;
    while p < ch.len() {
        if ch[p] == q {
            // Greedy `QQ` pair with backtrack: the pair is taken iff a
            // closing quote follows the pair (else close at this quote).
            if p + 1 < ch.len() && ch[p + 1] == q && ch[p + 2..].contains(&q) {
                p += 2;
                continue;
            }
            return Some(p + 1 - pos);
        } else if ch[p] == '\\' {
            // Greedy `\\Q` escape with backtrack: the escape is taken iff
            // another quote follows (otherwise the star backtracks, the
            // backslash goes through `[^Q]`, and this quote closes).
            if p + 1 < ch.len() && ch[p + 1] == q && ch[p + 2..].contains(&q) {
                p += 2;
            } else {
                p += 1;
            }
        } else {
            p += 1;
        }
    }
    None
}

/// Match one SQL_REGEX rule (by index); return char length on success.
/// Indices follow `keywords.SQL_REGEX`; see `PATTERN_SOURCES`.
fn match_rule(idx: usize, ch: &[char], pos: usize) -> Option<usize> {
    let c = |i: usize| *ch.get(i).unwrap_or(&'\0');
    match idx {
        // 0: (-- | # )\+ minimal-to-newline Hint comment (newline consumed).
        0 => {
            let mut p = if c(pos) == '-' && c(pos + 1) == '-' {
                pos + 2
            } else if c(pos) == '#' && c(pos + 1) == ' ' {
                pos + 2
            } else {
                return None;
            };
            if c(p) != '+' {
                return None;
            }
            p += 1;
            while p < ch.len() && ch[p] != '\n' && ch[p] != '\r' {
                p += 1;
            }
            if p < ch.len() {
                p += 1;
                if ch[p - 1] == '\r' && p < ch.len() && ch[p] == '\n' {
                    p += 1;
                }
            }
            Some(p - pos)
        }
        // 1: (-- | # ) minimal-to-newline Single comment.
        1 => {
            let mut p = if c(pos) == '-' && c(pos + 1) == '-' {
                pos + 2
            } else if c(pos) == '#' && c(pos + 1) == ' ' {
                pos + 2
            } else {
                return None;
            };
            while p < ch.len() && ch[p] != '\n' && ch[p] != '\r' {
                p += 1;
            }
            if p < ch.len() {
                p += 1;
                if ch[p - 1] == '\r' && p < ch.len() && ch[p] == '\n' {
                    p += 1;
                }
            }
            Some(p - pos)
        }
        // 2: single newline (\r\n is one token).
        2 => {
            if c(pos) == '\r' && c(pos + 1) == '\n' {
                Some(2)
            } else if c(pos) == '\r' || c(pos) == '\n' {
                Some(1)
            } else {
                None
            }
        }
        // 3: \s+? lazy = exactly one whitespace char.
        3 => (is_ws(c(pos)) && pos < ch.len()).then_some(1),
        // 4: := assignment.
        4 => (c(pos) == ':' && c(pos + 1) == '=').then_some(2),
        // 5: :: punctuation.
        5 => (c(pos) == ':' && c(pos + 1) == ':').then_some(2),
        // 6: * wildcard.
        6 => (pos < ch.len() && c(pos) == '*').then_some(1),
        // 7/8: quoted names with doubled-quote escapes.
        7 | 8 => {
            let q = if idx == 7 { '`' } else { '´' };
            if c(pos) != q || pos >= ch.len() {
                return None;
            }
            let mut p = pos + 1;
            while p < ch.len() {
                if ch[p] == q {
                    if c(p + 1) == q {
                        p += 2;
                        continue;
                    }
                    return Some(p + 1 - pos);
                }
                p += 1;
            }
            None
        }
        // 9: ? placeholder.
        9 => (pos < ch.len() && c(pos) == '?').then_some(1),
        // 10: %(name)s / %s placeholder.
        10 => {
            if c(pos) != '%' || pos >= ch.len() {
                return None;
            }
            let mut p = pos + 1;
            if c(p) == '(' {
                p += 1;
                let w = word_run(ch, p);
                if w == p {
                    return None;
                }
                p = w;
                if c(p) != ')' {
                    return None;
                }
                p += 1;
            }
            (c(p) == 's').then(|| p + 1 - pos)
        }
        // 11: (?<!\w)[$:?]\w+ placeholder.
        11 => {
            if pos < ch.len()
                && (c(pos) == '$' || c(pos) == ':' || c(pos) == '?')
                && (pos == 0 || !is_word(ch[pos - 1]))
            {
                let w = word_run(ch, pos + 1);
                if w > pos + 1 {
                    return Some(w - pos);
                }
            }
            None
        }
        // 12: \\\w+ command.
        12 => {
            if pos < ch.len() && c(pos) == '\\' {
                let w = word_run(ch, pos + 1);
                if w > pos + 1 {
                    return Some(w - pos);
                }
            }
            None
        }
        // 13: (CASE|IN|VALUES|USING|FROM|AS)\b keyword.
        13 => {
            for w in ["CASE", "IN", "VALUES", "USING", "FROM", "AS"] {
                if let Some(e) = lit(ch, pos, w)
                    && boundary(ch, e)
                {
                    return Some(e - pos);
                }
            }
            None
        }
        // 14: (@|##|#)[A-ZÀ-Ü]\w* name.
        14 => {
            let p = if c(pos) == '@' {
                pos + 1
            } else if c(pos) == '#' && c(pos + 1) == '#' {
                pos + 2
            } else if c(pos) == '#' {
                pos + 1
            } else {
                return None;
            };
            if p < ch.len() && in_az(ch[p]) {
                let w = word_run(ch, p + 1);
                if w > p + 1 {
                    return Some(w - pos);
                }
            }
            None
        }
        // 15: [A-ZÀ-Ü]\w*(?=\s*\.(?!\d)) name.
        15 => {
            if pos < ch.len() && in_az(ch[pos]) {
                let e = word_run(ch, pos + 1);
                let mut p = e;
                while p < ch.len() && is_ws(ch[p]) {
                    p += 1;
                }
                if p < ch.len() && ch[p] == '.' && !c(p + 1).to_digit(10).is_some() {
                    return Some(e - pos);
                }
            }
            None
        }
        // 16: (?<=\.)[A-ZÀ-Ü]\w* name.
        16 => {
            if pos > 0 && ch[pos - 1] == '.' && pos < ch.len() && in_az(ch[pos]) {
                Some(word_run(ch, pos + 1) - pos)
            } else {
                None
            }
        }
        // 17: [A-ZÀ-Ü]\w*(?=\() name.
        17 => {
            if pos < ch.len() && in_az(ch[pos]) {
                let e = word_run(ch, pos + 1);
                if e < ch.len() && ch[e] == '(' {
                    return Some(e - pos);
                }
            }
            None
        }
        // 18: -?0x[\dA-F]+ hexadecimal.
        18 => {
            let mut p = pos;
            if c(p) == '-' {
                p += 1;
            }
            if !(c(p) == '0' && (c(p + 1) == 'x' || c(p + 1) == 'X')) {
                return None;
            }
            p += 2;
            let mut n = 0;
            while p < ch.len() && (is_digit(ch[p]) || matches!(fold_lo(ch[p]), 'a'..='f')) {
                p += 1;
                n += 1;
            }
            (n > 0).then(|| p - pos)
        }
        // 19: -?\d+(\.\d+)?E-?\d+ float (no '+' after E, like upstream).
        19 => {
            let mut p = pos;
            if c(p) == '-' {
                p += 1;
            }
            let d0 = digit_run(ch, p);
            if d0 == p {
                return None;
            }
            p = d0;
            if c(p) == '.' {
                let d1 = digit_run(ch, p + 1);
                if d1 == p + 1 {
                    return None;
                }
                p = d1;
            }
            if c(p) != 'E' && c(p) != 'e' {
                return None;
            }
            p += 1;
            if c(p) == '-' {
                p += 1;
            }
            let d2 = digit_run(ch, p);
            if d2 == p {
                return None;
            }
            Some(d2 - pos)
        }
        // 20: (?![_A-ZÀ-Ü])-?(\d+(\.\d*)|\.\d+)(?![_A-ZÀ-Ü]) float.
        20 => {
            if pos < ch.len() && in_neg_class(ch[pos]) {
                return None;
            }
            let mut p = pos;
            if c(p) == '-' {
                p += 1;
            }
            // Alt 1: \d+(\.\d*) — the dot group is mandatory; \d*
            // backtracks to empty on guard failure ('12.5a' -> '12.').
            if is_digit(c(p)) {
                let d = digit_run(ch, p);
                if c(d) == '.' {
                    let mut q = digit_run(ch, d + 1);
                    loop {
                        if q >= ch.len() || !in_neg_class(ch[q]) {
                            return Some(q - pos);
                        }
                        if q == d + 1 {
                            break;
                        }
                        q -= 1;
                    }
                }
            }
            // Alt 2: \.\d+ with the same trailing-guard backtrack
            // ('.140x1F' -> '.14').
            if c(p) == '.' && is_digit(c(p + 1)) {
                let mut q = digit_run(ch, p + 1);
                loop {
                    if q >= ch.len() || !in_neg_class(ch[q]) {
                        return Some(q - pos);
                    }
                    // Floor: keep at least one digit ('\.\d+' needs 2+ chars).
                    if q == p + 2 {
                        break;
                    }
                    q -= 1;
                }
            }
            None
        }
        // 21: (?![_A-ZÀ-Ü])-?\d+(?![_A-ZÀ-Ü]) integer.
        21 => {
            if pos < ch.len() && in_neg_class(ch[pos]) {
                return None;
            }
            let mut p = pos;
            if c(p) == '-' {
                p += 1;
            }
            let d = digit_run(ch, p);
            // Greedy \d+ backtracks to the longest prefix passing the
            // trailing guard ('12ab' -> '1'; '123abc456' -> '12').
            let mut q = d;
            loop {
                if q >= ch.len() || !in_neg_class(ch[q]) {
                    return if q > p { Some(q - pos) } else { None };
                }
                if q == p {
                    return None;
                }
                q -= 1;
            }
        }
        // 22: '(?:''|\\'|[^'])*' single string.
        22 => match_quoted(ch, pos, '\''),
        // 23: "(?:""|\\"|[^"])*" symbol string.
        23 => match_quoted(ch, pos, '"'),
        // 24: (""|".*?[^\\]") symbol string.
        24 => {
            if pos >= ch.len() || ch[pos] != '"' {
                return None;
            }
            if c(pos + 1) == '"' {
                return Some(2);
            }
            let mut i = pos + 1;
            while i < ch.len() {
                if ch[i] == '"' && ch[i - 1] != '\\' {
                    return Some(i + 1 - pos);
                }
                i += 1;
            }
            None
        }
        // 25: (?<![\w\])])(\[[^\]\[]+\]) bracket name.
        25 => {
            if pos > 0 && (is_word(ch[pos - 1]) || ch[pos - 1] == ']' || ch[pos - 1] == ')')
            {
                return None;
            }
            if c(pos) != '[' {
                return None;
            }
            let mut p = pos + 1;
            while p < ch.len() && ch[p] != ']' && ch[p] != '[' {
                p += 1;
            }
            if p > pos + 1 && p < ch.len() && ch[p] == ']' {
                Some(p + 1 - pos)
            } else {
                None
            }
        }
        // 26: JOIN with optional LEFT/RIGHT/FULL + INNER/OUTER/STRAIGHT,
        // or CROSS/NATURAL prefix; trailing \b.
        26 => {
            let mut p = pos;
            for a in ["LEFT", "RIGHT", "FULL"] {
                if let Some(e) = lit(ch, p, a)
                    && let Some(w) = ws1(ch, e)
                {
                    p = w;
                    break;
                }
            }
            let mut q = p;
            for b in ["INNER", "OUTER", "STRAIGHT"] {
                if let Some(e) = lit(ch, p, b)
                    && let Some(w) = ws1(ch, e)
                {
                    q = w;
                    break;
                }
            }
            if let Some(e) = lit(ch, q, "JOIN")
                && boundary(ch, e)
            {
                return Some(e - pos);
            }
            let mut p = pos;
            for cc in ["CROSS", "NATURAL"] {
                if let Some(e) = lit(ch, p, cc)
                    && let Some(w) = ws1(ch, e)
                {
                    p = w;
                    break;
                }
            }
            if let Some(e) = lit(ch, p, "JOIN")
                && boundary(ch, e)
            {
                return Some(e - pos);
            }
            None
        }
        // 27: END(\s+IF|\s+LOOP|\s+WHILE|\s+FOR|\s+CASE)?\b.
        27 => {
            let e = lit(ch, pos, "END")?;
            for s in ["IF", "LOOP", "WHILE", "FOR", "CASE"] {
                if let Some(w) = ws1(ch, e)
                    && let Some(se) = lit(ch, w, s)
                    && boundary(ch, se)
                {
                    return Some(se - pos);
                }
            }
            boundary(ch, e).then(|| e - pos)
        }
        // 28: IF\s+(NOT\s+)?EXISTS\b.
        28 => {
            let e = lit(ch, pos, "IF")?;
            let w = ws1(ch, e)?;
            let w2 = lit(ch, w, "NOT").and_then(|ne| ws1(ch, ne)).unwrap_or(w);
            let ee = lit(ch, w2, "EXISTS")?;
            boundary(ch, ee).then(|| ee - pos)
        }
        // 29: NOT\s+NULL\b.
        29 => {
            let e = lit(ch, pos, "NOT")?;
            let w = ws1(ch, e)?;
            let ee = lit(ch, w, "NULL")?;
            boundary(ch, ee).then(|| ee - pos)
        }
        // 30: (ASC|DESC)(\s+NULLS\s+(FIRST|LAST))?\b order.
        30 => {
            for a in ["ASC", "DESC"] {
                if let Some(e) = lit(ch, pos, a) {
                    if let Some(w) = ws1(ch, e)
                        && let Some(ne) = lit(ch, w, "NULLS")
                        && let Some(w2) = ws1(ch, ne)
                    {
                        for f in ["FIRST", "LAST"] {
                            if let Some(fe) = lit(ch, w2, f)
                                && boundary(ch, fe)
                            {
                                return Some(fe - pos);
                            }
                        }
                    }
                    if boundary(ch, e) {
                        return Some(e - pos);
                    }
                }
            }
            None
        }
        // 31: (ASC|DESC)\b order.
        31 => {
            for a in ["ASC", "DESC"] {
                if let Some(e) = lit(ch, pos, a)
                    && boundary(ch, e)
                {
                    return Some(e - pos);
                }
            }
            None
        }
        // 32: NULLS\s+(FIRST|LAST)\b order.
        32 => {
            let e = lit(ch, pos, "NULLS")?;
            let w = ws1(ch, e)?;
            for f in ["FIRST", "LAST"] {
                if let Some(fe) = lit(ch, w, f)
                    && boundary(ch, fe)
                {
                    return Some(fe - pos);
                }
            }
            None
        }
        // 33: UNION\s+ALL\b.
        33 => {
            let e = lit(ch, pos, "UNION")?;
            let w = ws1(ch, e)?;
            let ee = lit(ch, w, "ALL")?;
            boundary(ch, ee).then(|| ee - pos)
        }
        // 34: CREATE(\s+OR\s+REPLACE)?\b DDL.
        34 => {
            let e = lit(ch, pos, "CREATE")?;
            if let Some(w) = ws1(ch, e)
                && let Some(oe) = lit(ch, w, "OR")
                && let Some(w2) = ws1(ch, oe)
                && let Some(re) = lit(ch, w2, "REPLACE")
                && boundary(ch, re)
            {
                return Some(re - pos);
            }
            boundary(ch, e).then(|| e - pos)
        }
        // 35: DOUBLE\s+PRECISION\b builtin.
        35 => {
            let e = lit(ch, pos, "DOUBLE")?;
            let w = ws1(ch, e)?;
            let ee = lit(ch, w, "PRECISION")?;
            boundary(ch, ee).then(|| ee - pos)
        }
        // 36: GROUP\s+BY\b. 37: ORDER\s+BY\b.
        36 => {
            let e = lit(ch, pos, "GROUP")?;
            let w = ws1(ch, e)?;
            let ee = lit(ch, w, "BY")?;
            boundary(ch, ee).then(|| ee - pos)
        }
        37 => {
            let e = lit(ch, pos, "ORDER")?;
            let w = ws1(ch, e)?;
            let ee = lit(ch, w, "BY")?;
            boundary(ch, ee).then(|| ee - pos)
        }
        // 38: PRIMARY\s+KEY\b. 39: HANDLER\s+FOR\b.
        38 => {
            let e = lit(ch, pos, "PRIMARY")?;
            let w = ws1(ch, e)?;
            let ee = lit(ch, w, "KEY")?;
            boundary(ch, ee).then(|| ee - pos)
        }
        39 => {
            let e = lit(ch, pos, "HANDLER")?;
            let w = ws1(ch, e)?;
            let ee = lit(ch, w, "FOR")?;
            boundary(ch, ee).then(|| ee - pos)
        }
        // 40: GO(\s\d)\b — exactly one whitespace char + one digit.
        40 => {
            let e = lit(ch, pos, "GO")?;
            if e < ch.len() && is_ws(ch[e]) && e + 1 < ch.len() && is_digit(ch[e + 1]) {
                let ee = e + 2;
                if boundary(ch, ee) {
                    return Some(ee - pos);
                }
            }
            None
        }
        // 41: (LATERAL\s+VIEW\s+)(EXPLODE|INLINE|PARSE_URL_TUPLE|POSEXPLODE|STACK)\b.
        41 => {
            let e = lit(ch, pos, "LATERAL")?;
            let w = ws1(ch, e)?;
            let ve = lit(ch, w, "VIEW")?;
            let w2 = ws1(ch, ve)?;
            for k in ["EXPLODE", "INLINE", "PARSE_URL_TUPLE", "POSEXPLODE", "STACK"] {
                if let Some(ke) = lit(ch, w2, k)
                    && boundary(ch, ke)
                {
                    return Some(ke - pos);
                }
            }
            None
        }
        // 42: (AT|WITH')\s+TIME\s+ZONE\s+'[^']+' TZCast.
        42 => {
            let e = if let Some(e) = lit(ch, pos, "AT") {
                e
            } else if pos + 5 <= ch.len()
                && eq_ci('W', ch[pos])
                && eq_ci('I', ch[pos + 1])
                && eq_ci('T', ch[pos + 2])
                && eq_ci('H', ch[pos + 3])
                && ch[pos + 4] == '\''
            {
                pos + 5
            } else {
                return None;
            };
            let w = ws1(ch, e)?;
            let te = lit(ch, w, "TIME")?;
            let w2 = ws1(ch, te)?;
            let ze = lit(ch, w2, "ZONE")?;
            let w3 = ws1(ch, ze)?;
            if w3 < ch.len() && ch[w3] == '\'' {
                let mut p = w3 + 1;
                while p < ch.len() && ch[p] != '\'' {
                    p += 1;
                }
                if p < ch.len() && p > w3 + 1 {
                    return Some(p + 1 - pos);
                }
            }
            None
        }
        // 43: (NOT\s+)?(LIKE|ILIKE|RLIKE)\b comparison.
        43 => {
            let base = lit(ch, pos, "NOT")
                .and_then(|e| ws1(ch, e))
                .unwrap_or(pos);
            for k in ["LIKE", "ILIKE", "RLIKE"] {
                if let Some(ke) = lit(ch, base, k)
                    && boundary(ch, ke)
                {
                    return Some(ke - pos);
                }
            }
            None
        }
        // 44: (NOT\s+)?(REGEXP)(\s+(BINARY))?\b comparison.
        44 => {
            let base = lit(ch, pos, "NOT")
                .and_then(|e| ws1(ch, e))
                .unwrap_or(pos);
            let re = lit(ch, base, "REGEXP")?;
            if let Some(w) = ws1(ch, re)
                && let Some(be) = lit(ch, w, "BINARY")
                && boundary(ch, be)
            {
                return Some(be - pos);
            }
            boundary(ch, re).then(|| re - pos)
        }
        // 45: \w[$#\w]* — handled in lex() (keyword lookup).
        45 => {
            if pos < ch.len() && is_word(ch[pos]) {
                let mut p = pos + 1;
                while p < ch.len() && (is_word(ch[p]) || ch[p] == '$' || ch[p] == '#') {
                    p += 1;
                }
                Some(p - pos)
            } else {
                None
            }
        }
        // 46: [;:()\[\],\.] punctuation.
        46 => {
            (pos < ch.len()
                && matches!(ch[pos], ';' | ':' | '(' | ')' | '[' | ']' | ',' | '.'))
            .then_some(1)
        }
        // 47: JSON operators (->>|->|#>>|#>|@>|<@|?|?|?&-forms|-|#-).
        // Alternation order: '?' + optional '|' before '?&' (first match wins,
        // so '?&' lexes as '?' + '&'; there is no bare '&' alternative).
        47 => {
            if pos >= ch.len() {
                return None;
            }
            match ch[pos] {
                '-' if c(pos + 1) == '>' => {
                    Some(if c(pos + 2) == '>' { 3 } else { 2 })
                }
                '-' => Some(1),
                '#' if c(pos + 1) == '>' => {
                    Some(if c(pos + 2) == '>' { 3 } else { 2 })
                }
                '#' if c(pos + 1) == '-' => Some(2),
                '@' if c(pos + 1) == '>' => Some(2),
                '<' if c(pos + 1) == '@' => Some(2),
                '?' => Some(if c(pos + 1) == '|' { 2 } else { 1 }),
                _ => None,
            }
        }
        // 48: [<>=~!]+ comparison.
        48 => {
            if pos < ch.len() && matches!(ch[pos], '<' | '>' | '=' | '~' | '!') {
                let mut p = pos + 1;
                while p < ch.len() && matches!(ch[p], '<' | '>' | '=' | '~' | '!') {
                    p += 1;
                }
                Some(p - pos)
            } else {
                None
            }
        }
        // 49: [+/@#%^&|^-]+ operator.
        49 => {
            if pos < ch.len()
                && matches!(ch[pos], '+' | '/' | '@' | '#' | '%' | '^' | '&' | '|' | '-')
            {
                let mut p = pos + 1;
                while p < ch.len()
                    && matches!(ch[p], '+' | '/' | '@' | '#' | '%' | '^' | '&' | '|' | '-')
                {
                    p += 1;
                }
                Some(p - pos)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Rule index to emitted `TType` (mirrors `SQL_REGEX` order; index 45 is
/// `PROCESS_AS_KEYWORD`, resolved per value in `lex`).
fn rule_ttype(idx: usize) -> TType {
    match idx {
        0 => TType::TOKEN_COMMENT_SINGLE_HINT,
        1 => TType::TOKEN_COMMENT_SINGLE,
        2 => TType::TOKEN_TEXT_WHITESPACE_NEWLINE,
        3 => TType::TOKEN_TEXT_WHITESPACE,
        4 => TType::TOKEN_ASSIGNMENT,
        5 => TType::TOKEN_PUNCTUATION,
        6 => TType::TOKEN_WILDCARD,
        7 | 8 => TType::TOKEN_NAME,
        9 | 10 | 11 => TType::TOKEN_NAME_PLACEHOLDER,
        12 => TType::TOKEN_GENERIC_COMMAND,
        13 => TType::TOKEN_KEYWORD,
        14 | 15 | 16 | 17 => TType::TOKEN_NAME,
        18 => TType::TOKEN_LITERAL_NUMBER_HEXADECIMAL,
        19 | 20 => TType::TOKEN_LITERAL_NUMBER_FLOAT,
        21 => TType::TOKEN_LITERAL_NUMBER_INTEGER,
        22 => TType::TOKEN_LITERAL_STRING_SINGLE,
        23 | 24 => TType::TOKEN_LITERAL_STRING_SYMBOL,
        25 => TType::TOKEN_NAME,
        26 | 27 | 28 | 29 => TType::TOKEN_KEYWORD,
        30 | 31 | 32 => TType::TOKEN_KEYWORD_ORDER,
        33 => TType::TOKEN_KEYWORD,
        34 => TType::TOKEN_KEYWORD_DDL,
        35 => TType::TOKEN_NAME_BUILTIN,
        36 | 37 | 38 | 39 | 40 | 41 => TType::TOKEN_KEYWORD,
        42 => TType::TOKEN_KEYWORD_TZCAST,
        43 | 44 => TType::TOKEN_OPERATOR_COMPARISON,
        46 => TType::TOKEN_PUNCTUATION,
        47 => TType::TOKEN_OPERATOR,
        48 => TType::TOKEN_OPERATOR_COMPARISON,
        49 => TType::TOKEN_OPERATOR,
        _ => TType::TOKEN_ERROR,
    }
}

/// One opener: (start, end, tag id, ttype). Tags: dollar delims are keyed
/// by their full `$tag$` span; all multiline comments share tag 0.
struct Opener {
    start: usize,
    end: usize,
    tag: usize,
    ttype: TType,
}

/// Pre-scan for dollar-quoted literals + multiline comments, mirroring
/// `keywords.find_delimited_spans` (single pass, overlap-aware).
struct SpanTable {
    openers: Vec<Opener>,
    /// (tag span start, tag span end, closer starts, closer ends).
    closers: Vec<(usize, usize, Vec<usize>, Vec<usize>)>,
}

impl SpanTable {
    /// Bucket id for a delimiter occurrence: occurrences sharing the same
    /// tag TEXT share a bucket (upstream `closers[tag]`); the stored span
    /// is just the first occurrence's.
    fn tag_id(&mut self, ch: &[char], s: usize, e: usize) -> usize {
        if let Some(i) = self
            .closers
            .iter()
            .position(|&(ts, te, _, _)| ch[ts..te] == ch[s..e])
        {
            return i;
        }
        self.closers.push((s, e, Vec::new(), Vec::new()));
        self.closers.len() - 1
    }

    fn resolve(&self, pos: usize) -> Option<(usize, TType)> {
        // Openers are pushed in ascending start order; linear scan is fine.
        let o = self.openers.iter().find(|o| o.start == pos)?;
        let (_, _, starts, ends) = &self.closers[o.tag];
        // bisect_left on starts for opener_end.
        let idx = starts.partition_point(|&s| s < o.end);
        if idx == starts.len() {
            return None;
        }
        Some((ends[idx], o.ttype))
    }
}

/// Try to parse a `$tag$` delimiter at `s`; return end on success.
/// Tag = optional `[_A-ZÀ-Ü]\w*` (IGNORECASE on the class), overlapping
/// attempts at every `$` like the lookahead scan upstream.
fn dollar_delim(ch: &[char], s: usize) -> Option<usize> {
    if ch.get(s) != Some(&'$') {
        return None;
    }
    let mut p = s + 1;
    if p < ch.len() && in_az(ch[p]) {
        p = word_run(ch, p + 1);
    }
    if ch.get(p) == Some(&'$') {
        Some(p + 1)
    } else {
        None
    }
}

fn find_spans(ch: &[char]) -> Option<SpanTable> {
    let has_dollar = ch.contains(&'$');
    let text_has = |a: char, b: char| {
        ch.windows(2).any(|w| w[0] == a && w[1] == b)
    };
    let has_comment_open = text_has('/', '*');
    if !has_dollar && !has_comment_open {
        return None;
    }
    let mut t = SpanTable { openers: Vec::new(), closers: Vec::new() };
    // Comment tag 0: reserve slot (span 0,0 = shared bucket).
    t.closers.push((0, 0, Vec::new(), Vec::new()));
    if has_dollar {
        for s in 0..ch.len() {
            if let Some(e) = dollar_delim(ch, s) {
                let tag = t.tag_id(ch, s, e);
                let (_, _, starts, ends) = &mut t.closers[tag];
                starts.push(s);
                ends.push(e);
                // Opener-ok: no preceding word char, `"` or `$`.
                let ok = s == 0 || {
                    let pc = ch[s - 1];
                    !(is_word(pc) || pc == '"' || pc == '$')
                };
                if ok {
                    t.openers.push(Opener { start: s, end: e, tag, ttype: TType::TOKEN_LITERAL });
                }
            }
        }
    }
    if has_comment_open {
        for s in 0..ch.len().saturating_sub(1) {
            if ch[s] == '/' && ch[s + 1] == '*' {
                if ch.get(s + 2) == Some(&'+') {
                    t.openers.push(Opener {
                        start: s,
                        end: s + 3,
                        tag: 0,
                        ttype: TType::TOKEN_COMMENT_MULTILINE_HINT,
                    });
                } else {
                    t.openers.push(Opener {
                        start: s,
                        end: s + 2,
                        tag: 0,
                        ttype: TType::TOKEN_COMMENT_MULTILINE,
                    });
                }
            }
            if ch[s] == '*' && ch[s + 1] == '/' {
                let (_, _, starts, ends) = &mut t.closers[0];
                starts.push(s);
                ends.push(s + 2);
            }
        }
    }
    Some(t)
}

/// Lexed output: input chars, byte offset per char (+sentinel), tokens as
/// byte spans (ready to slice `&str` without re-walking).
pub struct Lexed {
    pub chars: Vec<char>,
    pub bytes: Vec<u32>,
    pub toks: Vec<RawTok>,
}

/// Default-configuration lex of the whole string (mirrors
/// `Lexer.get_default_instance().get_tokens` on `str` input).
pub fn lex(text: &str) -> Lexed {
    let chars: Vec<char> = text.chars().collect();
    let mut bytes: Vec<u32> = Vec::with_capacity(chars.len() + 1);
    let mut off = 0u32;
    for &c in &chars {
        bytes.push(off);
        off += c.len_utf8() as u32;
    }
    bytes.push(off);
    let spans = find_spans(&chars);
    let mut toks: Vec<RawTok> = Vec::new();
    let mut pos = 0;
    while pos < chars.len() {
        if let Some(t) = spans.as_ref()
            && let Some((end, ttype)) = t.resolve(pos)
        {
            toks.push(RawTok { ttype, start: bytes[pos], end: bytes[end] });
            pos = end;
            continue;
        }
        let mut done = false;
        for idx in 0..50 {
            if idx == 45 {
                continue; // keyword rule handled below, in order
            }
            if let Some(len) = match_rule(idx, &chars, pos) {
                debug_assert!(len > 0);
                toks.push(RawTok {
                    ttype: rule_ttype(idx),
                    start: bytes[pos],
                    end: bytes[pos + len],
                });
                pos += len;
                done = true;
                break;
            }
        }
        if done {
            continue;
        }
        // Rule 45: \w[$#\w]* → keyword lookup, else Name.
        if let Some(len) = match_rule(45, &chars, pos) {
            let value: String = chars[pos..pos + len].iter().collect();
            let ttype = is_keyword(&value).unwrap_or(TType::TOKEN_NAME);
            toks.push(RawTok { ttype, start: bytes[pos], end: bytes[pos + len] });
            pos += len;
            continue;
        }
        // Rules 46-49 in order, else single-char Error.
        let mut fell = true;
        for idx in 46..50 {
            if let Some(len) = match_rule(idx, &chars, pos) {
                toks.push(RawTok {
                    ttype: rule_ttype(idx),
                    start: bytes[pos],
                    end: bytes[pos + len],
                });
                pos += len;
                fell = false;
                break;
            }
        }
        if fell {
            toks.push(RawTok {
                ttype: TType::TOKEN_ERROR,
                start: bytes[pos],
                end: bytes[pos + 1],
            });
            pos += 1;
        }
    }
    Lexed { chars, bytes, toks }
}

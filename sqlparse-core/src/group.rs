//! Grouping engine mirroring `engine/grouping.py`.
//!
//! All 26 passes in `group()` order. `_group_matching`/`_group` carry the
//! explicit depth/token caps (exact messages via `CoreError`);
//! `@recurse`-style traversals are iterative post-order (no stack risk,
//! exact skip-subtree semantics). Keyword/value comparisons always use the
//! real input text (captured, never `""`).

use crate::tables_gen::TType;
use crate::tree::*;

type GRes = Result<(), CoreError>;

fn toks_err(kids: &[Node]) -> Option<CoreError> {
    if kids.len() > MAX_GROUPING_TOKENS {
        Some(CoreError::Tokens)
    } else {
        None
    }
}

fn group_matching(
    text: &str,
    kids: &mut Vec<Node>,
    kind: GroupKind,
    open: (TType, MatchVal<'_>),
    close: (TType, MatchVal<'_>),
    depth: u32,
) -> GRes {
    if depth > MAX_GROUPING_DEPTH {
        return Err(CoreError::Depth);
    }
    if let Some(e) = toks_err(kids) {
        return Err(e);
    }
    let mut opens: Vec<usize> = Vec::new();
    let mut pos = 0;
    while pos < kids.len() {
        if is_ws(&kids[pos]) {
            pos += 1;
            continue;
        }
        if kids[pos].is_group()
            && !matches!(&kids[pos], Node::Grp { kind: k, .. } if *k == kind)
        {
            if let Node::Grp { kids: sub, .. } = &mut kids[pos] {
                group_matching(
                    text,
                    sub,
                    kind,
                    (open.0, match_val_clone(&open.1)),
                    (close.0, match_val_clone(&close.1)),
                    depth + 1,
                )?;
            }
            pos += 1;
            continue;
        }
        if tok_match(text, &kids[pos], open.0, match_val_clone(&open.1)) {
            opens.push(pos);
        } else if tok_match(text, &kids[pos], close.0, match_val_clone(&close.1)) {
            if let Some(open_idx) = opens.pop() {
                let close_idx = pos;
                group_tokens(kids, kind, open_idx, close_idx, false);
                pos = open_idx + 1;
                continue;
            }
        }
        pos += 1;
    }
    Ok(())
}

fn match_val_clone<'a>(v: &MatchVal<'a>) -> MatchVal<'a> {
    match v {
        MatchVal::None => MatchVal::None,
        MatchVal::One(s) => MatchVal::One(s),
        MatchVal::Many(vs) => MatchVal::Many(vs),
    }
}

/// Generic join pass mirroring `_group`, including its snapshot iteration:
/// `token_list = list(tlist)` is frozen at entry while `tidx = idx - offset`
/// maps into the live list, so consumed (stale) tokens keep voting on
/// `match`/valid and reset `pidx` — the nesting mechanism. `vp`/`vn` close
/// over the input text; `post` receives it explicitly.
#[allow(clippy::too_many_arguments)]
fn group(
    text: &str,
    kids: &mut Vec<Node>,
    kind: GroupKind,
    m: &dyn Fn(&str, &Node) -> bool,
    vp: &dyn Fn(Option<&Node>) -> bool,
    vn: &dyn Fn(Option<&Node>) -> bool,
    post: &dyn Fn(&str, &mut Vec<Node>, usize, usize, Option<usize>) -> (usize, usize),
    extend: bool,
    recurse_flag: bool,
    depth: u32,
) -> GRes {
    if depth > MAX_GROUPING_DEPTH {
        return Err(CoreError::Depth);
    }
    if let Some(e) = toks_err(kids) {
        return Err(e);
    }
    // Frozen value snapshot (spans only — cheap); live nav by mapped index.
    let snap: Vec<Node> = kids.clone();
    let mut offset: isize = 0;
    let mut pidx: Option<usize> = None;
    let mut prev: Option<Node> = None;
    for (idx, stok) in snap.iter().enumerate() {
        let ti = idx as isize - offset;
        if ti < 0 {
            continue;
        }
        let ti = ti as usize;
        if is_ws_node(stok) {
            continue;
        }
        if recurse_flag && stok.is_group() && !matches!(stok, Node::Grp { kind: k, .. } if *k == kind) {
            let gid = match stok {
                Node::Grp { id, .. } => *id,
                _ => unreachable!(),
            };
            if let Some(sub) = find_group_kids(kids, gid) {
                group(text, sub, kind, m, vp, vn, post, extend, true, depth + 1)?;
            }
        }
        if m(text, stok) {
            let nidx = token_next(kids, Some(ti), true, false);
            let prev_ok = prev.as_ref().is_some_and(|p| vp(Some(p)));
            let next_ok = vn(nidx.map(|i| &kids[i]));
            if prev.is_some() && prev_ok && next_ok {
                let (from, to) = post(text, kids, pidx.unwrap(), ti, nidx);
                group_tokens(kids, kind, from, to, extend);
                offset += to as isize - from as isize;
                pidx = Some(from);
                prev = Some(kids[from].clone());
                continue;
            }
        }
        pidx = Some(ti);
        prev = Some(stok.clone());
    }
    Ok(())
}

fn is_ws_node(n: &Node) -> bool {
    matches!(n, Node::Tok { ttype, .. } if ttype.is_in(TType::TOKEN_TEXT_WHITESPACE))
}

/// Locate a live group subtree by stable id (DFS; absorbed groups nest).
fn find_group_kids<'a>(kids: &'a mut Vec<Node>, id: u64) -> Option<&'a mut Vec<Node>> {
    for n in kids.iter_mut() {
        if let Node::Grp { id: gid, kids: sub, .. } = n {
            if *gid == id {
                return Some(sub);
            }
            if let Some(r) = find_group_kids(sub, id) {
                return Some(r);
            }
        }
    }
    None
}

/// Iterative post-order `@recurse` traversal: apply `f` to every level
/// except pruned `skip` subtrees (children before parents, in order),
/// passing each level's own group kind (needed for `_groupable_tokens`).
fn recurse(
    text: &str,
    kind: GroupKind,
    kids: &mut Vec<Node>,
    skip: &[GroupKind],
    f: &dyn Fn(&str, GroupKind, &mut Vec<Node>) -> GRes,
) -> GRes {
    let mut idxs: Vec<usize> = Vec::new();
    for (i, n) in kids.iter().enumerate() {
        if let Node::Grp { kind: k, .. } = n {
            if !skip.contains(k) {
                idxs.push(i);
            }
        }
    }
    for i in idxs {
        if let Node::Grp { kind: k, kids: sub, .. } = &mut kids[i] {
            let k = *k;
            recurse(text, k, sub, skip, f)?;
        }
    }
    f(text, kind, kids)
}

// Shared filter sets (all 'static for the filter enums).
const I_SQ_ID: &[GroupKind] = &[GroupKind::SquareBrackets, GroupKind::Identifier];
const I_SQ_ID_FN: &[GroupKind] =
    &[GroupKind::SquareBrackets, GroupKind::Identifier, GroupKind::Function];
const I_PAR_FN_ID_OP_CMP: &[GroupKind] = &[
    GroupKind::Parenthesis,
    GroupKind::Function,
    GroupKind::Case,
    GroupKind::Identifier,
    GroupKind::Operation,
    GroupKind::Comparison,
];
const I_SQ_PAR_FN_ID_OP_TL: &[GroupKind] = &[
    GroupKind::SquareBrackets,
    GroupKind::Parenthesis,
    GroupKind::Function,
    GroupKind::Identifier,
    GroupKind::Operation,
    GroupKind::TypedLiteral,
];
const I_ID_LIST: &[GroupKind] = &[
    GroupKind::Function,
    GroupKind::Case,
    GroupKind::Identifier,
    GroupKind::Comparison,
    GroupKind::IdentifierList,
    GroupKind::Operation,
];
const T_ID_NUM: &[TType] = &[
    TType::TOKEN_LITERAL_NUMBER,
    TType::TOKEN_LITERAL_NUMBER_INTEGER,
    TType::TOKEN_LITERAL_NUMBER_FLOAT,
    TType::TOKEN_LITERAL_STRING,
    TType::TOKEN_LITERAL_STRING_SINGLE,
    TType::TOKEN_LITERAL_STRING_SYMBOL,
    TType::TOKEN_NAME,
    TType::TOKEN_NAME_PLACEHOLDER,
];
const T_ID_LIST: &[TType] = &[
    TType::TOKEN_LITERAL_NUMBER,
    TType::TOKEN_LITERAL_NUMBER_INTEGER,
    TType::TOKEN_LITERAL_NUMBER_FLOAT,
    TType::TOKEN_LITERAL_STRING,
    TType::TOKEN_LITERAL_STRING_SINGLE,
    TType::TOKEN_LITERAL_STRING_SYMBOL,
    TType::TOKEN_NAME,
    TType::TOKEN_NAME_PLACEHOLDER,
    TType::TOKEN_KEYWORD,
    TType::TOKEN_COMMENT,
    TType::TOKEN_WILDCARD,
];
const M_FOR: &[&str] = &["FOR", "FOREACH"];
const M_DAYS: &[&str] = &["DAY", "HOUR", "MINUTE", "MONTH", "SECOND", "YEAR"];
const M_WHERE_CLOSE: &[&str] = &[
    "ORDER BY",
    "GROUP BY",
    "LIMIT",
    "UNION",
    "UNION ALL",
    "EXCEPT",
    "INTERSECT",
    "HAVING",
    "RETURNING",
    "INTO",
];
const M_CUR: &[&str] = &["CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP"];
const M_TYPED_OPEN: &[(TType, MatchVal<'static>)] = &[
    (TType::TOKEN_NAME_BUILTIN, MatchVal::None),
    (TType::TOKEN_KEYWORD, MatchVal::One("TIMESTAMP")),
];

fn group_comments(text: &str, kids: &mut Vec<Node>) -> GRes {
    recurse(text, GroupKind::Statement, kids, &[GroupKind::Comment], &|text, _, kids| {
        let mut tidx = token_next_by(
            text, kids, ImtI::None, ImtM::None,
            ImtT::One(TType::TOKEN_COMMENT), -1, None,
        );
        while let Some(t) = tidx {
            let eidx = token_not_matching(
                kids,
                &|n| {
                    imt(text, Some(n), ImtI::One(GroupKind::Comment), ImtM::None, ImtT::One(TType::TOKEN_COMMENT))
                        || is_nl(n)
                },
                Some(t),
            );
            let e = match eidx {
                // No terminator ahead: every later start would hit the same
                // dead end (upstream O(n^2) guard).
                None => break,
                Some(e) => e,
            };
            let eidx2 = token_prev(kids, e, false, false).unwrap();
            group_tokens(kids, GroupKind::Comment, t, eidx2, false);
            tidx = token_next_by(
                text, kids, ImtI::None, ImtM::None,
                ImtT::One(TType::TOKEN_COMMENT), t as isize, None,
            );
        }
        Ok(())
    })
}

fn group_brackets(text: &str, kids: &mut Vec<Node>) -> GRes {
    group_matching(
        text, kids, GroupKind::SquareBrackets,
        (TType::TOKEN_PUNCTUATION, MatchVal::One("[")),
        (TType::TOKEN_PUNCTUATION, MatchVal::One("]")), 0,
    )
}

fn group_parenthesis(text: &str, kids: &mut Vec<Node>) -> GRes {
    group_matching(
        text, kids, GroupKind::Parenthesis,
        (TType::TOKEN_PUNCTUATION, MatchVal::One("(")),
        (TType::TOKEN_PUNCTUATION, MatchVal::One(")")), 0,
    )
}

fn group_case(text: &str, kids: &mut Vec<Node>) -> GRes {
    group_matching(
        text, kids, GroupKind::Case,
        (TType::TOKEN_KEYWORD, MatchVal::One("CASE")),
        (TType::TOKEN_KEYWORD, MatchVal::One("END")), 0,
    )
}

fn group_if(text: &str, kids: &mut Vec<Node>) -> GRes {
    group_matching(
        text, kids, GroupKind::If,
        (TType::TOKEN_KEYWORD, MatchVal::One("IF")),
        (TType::TOKEN_KEYWORD, MatchVal::One("END IF")), 0,
    )
}

fn group_for(text: &str, kids: &mut Vec<Node>) -> GRes {
    group_matching(
        text, kids, GroupKind::For,
        (TType::TOKEN_KEYWORD, MatchVal::Many(M_FOR)),
        (TType::TOKEN_KEYWORD, MatchVal::One("END LOOP")), 0,
    )
}

fn group_begin(text: &str, kids: &mut Vec<Node>) -> GRes {
    group_matching(
        text, kids, GroupKind::Begin,
        (TType::TOKEN_KEYWORD, MatchVal::One("BEGIN")),
        (TType::TOKEN_KEYWORD, MatchVal::One("END")), 0,
    )
}

fn some_node(n: Option<&Node>) -> bool {
    n.is_some()
}

fn group_typecasts(text: &str, kids: &mut Vec<Node>) -> GRes {
    group(
        text, kids, GroupKind::Identifier,
        &|t, n| tok_match(t, n, TType::TOKEN_PUNCTUATION, MatchVal::One("::")),
        &some_node, &some_node,
        &|_, _, p, _, n| (p, n.unwrap()), true, true, 0,
    )
}

fn group_tzcasts(text: &str, kids: &mut Vec<Node>) -> GRes {
    group(
        text, kids, GroupKind::Identifier,
        &|_, n| matches!(n, Node::Tok { ttype, .. } if *ttype == TType::TOKEN_KEYWORD_TZCAST),
        &some_node,
        &|n| match n {
            None => false,
            Some(x) => {
                is_ws(x)
                    || tok_match(text, x, TType::TOKEN_KEYWORD, MatchVal::One("AS"))
                    || tok_match(text, x, TType::TOKEN_LITERAL_STRING_SINGLE, MatchVal::None)
            }
        },
        &|_, _, p, _, n| (p, n.unwrap()), true, true, 0,
    )
}

fn group_typed_literal(text: &str, kids: &mut Vec<Node>) -> GRes {
    group(
        text, kids, GroupKind::TypedLiteral,
        &|t, n| {
            imt(t, Some(n), ImtI::None, ImtM::Many(M_TYPED_OPEN), ImtT::None)
        },
        &some_node,
        &|n| {
            imt("", n, ImtI::None,
                ImtM::One(TType::TOKEN_LITERAL_STRING_SINGLE, MatchVal::None), ImtT::None)
        },
        &|_, _, _, t, n| (t, n.unwrap()), false, true, 0,
    )?;
    group(
        text, kids, GroupKind::TypedLiteral,
        &|_, n| matches!(n, Node::Grp { kind: GroupKind::TypedLiteral, .. }),
        &some_node,
        &|n| {
            imt(text, n, ImtI::None,
                ImtM::One(TType::TOKEN_KEYWORD, MatchVal::Many(M_DAYS)), ImtT::None)
        },
        &|_, _, _, t, n| (t, n.unwrap()), true, true, 0,
    )
}

fn group_period(text: &str, kids: &mut Vec<Node>) -> GRes {
    group(
        text, kids, GroupKind::Identifier,
        &|t, n| {
            tok_match(t, n, TType::TOKEN_PUNCTUATION, MatchVal::One("."))
                || tok_match(t, n, TType::TOKEN_OPERATOR, MatchVal::One("->"))
                || tok_match(t, n, TType::TOKEN_OPERATOR, MatchVal::One("->>"))
        },
        &|n| {
            imt("", n, ImtI::Many(I_SQ_ID), ImtM::None,
                ImtT::Many(&[TType::TOKEN_NAME, TType::TOKEN_LITERAL_STRING_SYMBOL]))
        },
        &|_| true,
        &|t, kids, p, ti, n| {
            let next = n.map(|i| &kids[i]);
            let ok = imt(
                t, next, ImtI::Many(I_SQ_ID_FN), ImtM::None,
                ImtT::Many(&[
                    TType::TOKEN_NAME,
                    TType::TOKEN_LITERAL_STRING_SYMBOL,
                    TType::TOKEN_WILDCARD,
                    TType::TOKEN_LITERAL_STRING_SINGLE,
                ]),
            );
            if ok { (p, n.unwrap()) } else { (p, ti) }
        },
        true, true, 0,
    )
}

fn group_as(text: &str, kids: &mut Vec<Node>) -> GRes {
    group(
        text, kids, GroupKind::Identifier,
        &|t, n| is_kw(n) && norm_eq(t, n, "AS"),
        &|n| match n {
            Some(x) if is_kw(x) => norm_eq(text, x, "NULL"),
            Some(x) => !is_kw(x),
            None => false,
        },
        &|n| match n {
            None => false,
            Some(x) => !imt(
                text, Some(x), ImtI::None, ImtM::None,
                ImtT::Many(&[
                    TType::TOKEN_KEYWORD_DML,
                    TType::TOKEN_KEYWORD_DDL,
                    TType::TOKEN_KEYWORD_CTE,
                ]),
            ),
        },
        &|_, _, p, _, n| (p, n.unwrap()), true, true, 0,
    )
}

fn group_assignment(text: &str, kids: &mut Vec<Node>) -> GRes {
    group(
        text, kids, GroupKind::Assignment,
        &|t, n| tok_match(t, n, TType::TOKEN_ASSIGNMENT, MatchVal::One(":=")),
        &|n| match n {
            Some(Node::Tok { ttype, .. }) => *ttype != TType::TOKEN_KEYWORD,
            Some(_) => true,
            None => false,
        },
        &|n| match n {
            Some(Node::Tok { ttype, .. }) => *ttype != TType::TOKEN_KEYWORD,
            Some(_) => true,
            None => false,
        },
        &|t, kids, p, _, n| {
            let sn = token_next_by(
                t, kids, ImtI::None,
                ImtM::One(TType::TOKEN_PUNCTUATION, MatchVal::One(";")),
                ImtT::None, n.unwrap() as isize, None,
            );
            (p, sn.unwrap_or_else(|| n.unwrap()))
        },
        true, true, 0,
    )
}

fn group_comparison(text: &str, kids: &mut Vec<Node>) -> GRes {
    group(
        text, kids, GroupKind::Comparison,
        &|_, n| {
            matches!(n, Node::Tok { ttype, .. } if *ttype == TType::TOKEN_OPERATOR_COMPARISON)
        },
        &|n| match n {
            None => false,
            Some(x) => {
                imt(text, Some(x), ImtI::Many(I_SQ_PAR_FN_ID_OP_TL), ImtM::None, ImtT::Many(T_ID_NUM))
                    || (is_kw(x) && norm_eq(text, x, "NULL"))
            }
        },
        &|n| match n {
            None => false,
            Some(x) => {
                imt(text, Some(x), ImtI::Many(I_SQ_PAR_FN_ID_OP_TL), ImtM::None, ImtT::Many(T_ID_NUM))
                    || (is_kw(x) && norm_eq(text, x, "NULL"))
            }
        },
        &|_, _, p, _, n| (p, n.unwrap()), false, true, 0,
    )
}

fn group_identifier(text: &str, kids: &mut Vec<Node>) -> GRes {
    recurse(text, GroupKind::Statement, kids, &[GroupKind::Identifier], &|text, _, kids| {
        let mut r = token_next_by(
            text, kids, ImtI::None, ImtM::None,
            ImtT::Many(&[TType::TOKEN_LITERAL_STRING_SYMBOL, TType::TOKEN_NAME]), -1, None,
        );
        while let Some(t) = r {
            group_tokens(kids, GroupKind::Identifier, t, t, false);
            r = token_next_by(
                text, kids, ImtI::None, ImtM::None,
                ImtT::Many(&[TType::TOKEN_LITERAL_STRING_SYMBOL, TType::TOKEN_NAME]),
                t as isize, None,
            );
        }
        Ok(())
    })
}

fn group_over(text: &str, kids: &mut Vec<Node>) -> GRes {
    recurse(text, GroupKind::Statement, kids, &[GroupKind::Over], &|text, _, kids| {
        let mut r = token_next_by(
            text, kids, ImtI::None,
            ImtM::One(TType::TOKEN_KEYWORD, MatchVal::One("OVER")), ImtT::None, -1, None,
        );
        while let Some(t) = r {
            if let Some(n) = token_next(kids, Some(t), true, false) {
                if imt(text, Some(&kids[n]), ImtI::One(GroupKind::Parenthesis), ImtM::None, ImtT::One(TType::TOKEN_NAME)) {
                    group_tokens(kids, GroupKind::Over, t, n, false);
                }
            }
            r = token_next_by(
                text, kids, ImtI::None,
                ImtM::One(TType::TOKEN_KEYWORD, MatchVal::One("OVER")), ImtT::None,
                t as isize, None,
            );
        }
        Ok(())
    })
}

fn group_arrays(text: &str, kids: &mut Vec<Node>) -> GRes {
    group(
        text, kids, GroupKind::Identifier,
        &|_, n| matches!(n, Node::Grp { kind: GroupKind::SquareBrackets, .. }),
        &|n| {
            imt("", n, ImtI::Many(I_SQ_ID_FN), ImtM::None,
                ImtT::Many(&[TType::TOKEN_NAME, TType::TOKEN_LITERAL_STRING_SYMBOL]))
        },
        &|_| true,
        &|_, _, p, t, _| (p, t), true, false, 0,
    )
}

fn group_operator(text: &str, kids: &mut Vec<Node>) -> GRes {
    group(
        text, kids, GroupKind::Operation,
        &|_, n| {
            imt("", Some(n), ImtI::None, ImtM::None,
                ImtT::Many(&[TType::TOKEN_OPERATOR, TType::TOKEN_WILDCARD]))
        },
        &|n| match n {
            None => false,
            Some(x) => {
                imt(text, Some(x), ImtI::Many(I_SQ_PAR_FN_ID_OP_TL), ImtM::None, ImtT::Many(T_ID_NUM))
                    || tok_match(text, x, TType::TOKEN_KEYWORD, MatchVal::Many(M_CUR))
            }
        },
        &|n| match n {
            None => false,
            Some(x) => {
                imt(text, Some(x), ImtI::Many(I_SQ_PAR_FN_ID_OP_TL), ImtM::None, ImtT::Many(T_ID_NUM))
                    || tok_match(text, x, TType::TOKEN_KEYWORD, MatchVal::Many(M_CUR))
            }
        },
        &|_, kids, p, t, n| {
            if let Node::Tok { ttype, .. } = &mut kids[t] {
                *ttype = TType::TOKEN_OPERATOR;
            }
            (p, n.unwrap())
        },
        false, true, 0,
    )
}

fn group_identifier_list(text: &str, kids: &mut Vec<Node>) -> GRes {
    group(
        text, kids, GroupKind::IdentifierList,
        &|t, n| tok_match(t, n, TType::TOKEN_PUNCTUATION, MatchVal::One(",")),
        &|n| {
            imt(text, n, ImtI::Many(I_ID_LIST),
                ImtM::One(TType::TOKEN_KEYWORD, MatchVal::Many(&["null", "role"])),
                ImtT::Many(T_ID_LIST))
        },
        &|n| {
            imt(text, n, ImtI::Many(I_ID_LIST),
                ImtM::One(TType::TOKEN_KEYWORD, MatchVal::Many(&["null", "role"])),
                ImtT::Many(T_ID_LIST))
        },
        &|_, _, p, _, n| (p, n.unwrap()), true, true, 0,
    )
}

fn group_where(text: &str, kids: &mut Vec<Node>) -> GRes {
    recurse(text, GroupKind::Statement, kids, &[GroupKind::Where], &|text, kind, kids| {
        let mut r = token_next_by(
            text, kids, ImtI::None,
            ImtM::One(TType::TOKEN_KEYWORD, MatchVal::One("WHERE")), ImtT::None, -1, None,
        );
        while let Some(t) = r {
            let e = token_next_by(
                text, kids, ImtI::None,
                ImtM::One(TType::TOKEN_KEYWORD, MatchVal::Many(M_WHERE_CLOSE)), ImtT::None,
                t as isize, None,
            );
            // `_groupable_tokens`: brackets exclude the brackets themselves.
            let off = kids.len() - groupable(kind, kids).len();
            let eidx = match e {
                None => off + (groupable(kind, kids).len() - 1),
                Some(ei) => ei - 1,
            };
            group_tokens(kids, GroupKind::Where, t, eidx, false);
            r = token_next_by(
                text, kids, ImtI::None,
                ImtM::One(TType::TOKEN_KEYWORD, MatchVal::One("WHERE")), ImtT::None,
                t as isize, None,
            );
        }
        Ok(())
    })
}

fn group_aliased(text: &str, kids: &mut Vec<Node>) -> GRes {
    recurse(text, GroupKind::Statement, kids, &[], &|text, _, kids| {
        let mut r = token_next_by(
            text, kids, ImtI::Many(I_PAR_FN_ID_OP_CMP), ImtM::None,
            ImtT::One(TType::TOKEN_LITERAL_NUMBER), -1, None,
        );
        while let Some(t) = r {
            if let Some(n) = token_next(kids, Some(t), true, false) {
                if matches!(&kids[n], Node::Grp { kind: GroupKind::Identifier, .. }) {
                    group_tokens(kids, GroupKind::Identifier, t, n, true);
                }
            }
            r = token_next_by(
                text, kids, ImtI::Many(I_PAR_FN_ID_OP_CMP), ImtM::None,
                ImtT::One(TType::TOKEN_LITERAL_NUMBER), t as isize, None,
            );
        }
        Ok(())
    })
}

fn group_functions(text: &str, kids: &mut Vec<Node>) -> GRes {
    recurse(text, GroupKind::Statement, kids, &[GroupKind::Function], &|text, _, kids| {
        let mut has_create = false;
        let mut has_table = false;
        let mut has_as = false;
        for n in kids.iter() {
            let v = match n {
                Node::Tok { .. } => n.value(text).to_uppercase(),
                Node::Grp { .. } => group_text(text, n).to_uppercase(),
            };
            if v == "CREATE" {
                has_create = true;
            }
            if v == "TABLE" {
                has_table = true;
            }
            if v == "AS" {
                has_as = true;
            }
        }
        if has_create && has_table && !has_as {
            return Ok(());
        }
        let mut r = token_next_by(
            text, kids, ImtI::None, ImtM::None, ImtT::One(TType::TOKEN_NAME), -1, None,
        );
        while let Some(t) = r {
            if let Some(n) = token_next(kids, Some(t), true, false) {
                if matches!(&kids[n], Node::Grp { kind: GroupKind::Parenthesis, .. }) {
                    let e = match token_next(kids, Some(n), true, false) {
                        Some(o)
                            if matches!(
                                &kids[o],
                                Node::Grp { kind: GroupKind::Over, .. }
                            ) =>
                        {
                            o
                        }
                        _ => n,
                    };
                    group_tokens(kids, GroupKind::Function, t, e, false);
                }
            }
            r = token_next_by(
                text, kids, ImtI::None, ImtM::None, ImtT::One(TType::TOKEN_NAME),
                t as isize, None,
            );
        }
        Ok(())
    })
}

fn group_text(text: &str, n: &Node) -> String {
    match n {
        Node::Tok { start, end, .. } => text[*start as usize..*end as usize].to_owned(),
        Node::Grp { kids, .. } => kids.iter().map(|k| group_text(text, k)).collect(),
    }
}

fn group_order(text: &str, kids: &mut Vec<Node>) -> GRes {
    recurse(text, GroupKind::Statement, kids, &[GroupKind::Identifier], &|text, _, kids| {
        let mut r = token_next_by(
            text, kids, ImtI::None, ImtM::None,
            ImtT::One(TType::TOKEN_KEYWORD_ORDER), -1, None,
        );
        while let Some(mut t) = r {
            if let Some(p) = token_prev(kids, t, true, false) {
                if imt(text, Some(&kids[p]), ImtI::One(GroupKind::Identifier), ImtM::None, ImtT::One(TType::TOKEN_LITERAL_NUMBER)) {
                    group_tokens(kids, GroupKind::Identifier, p, t, false);
                    t = p;
                }
            }
            r = token_next_by(
                text, kids, ImtI::None, ImtM::None,
                ImtT::One(TType::TOKEN_KEYWORD_ORDER), t as isize, None,
            );
        }
        Ok(())
    })
}

fn align_comments(text: &str, kids: &mut Vec<Node>) -> GRes {
    recurse(text, GroupKind::Statement, kids, &[], &|text, _, kids| {
        let mut r = token_next_by(
            text, kids, ImtI::One(GroupKind::Comment), ImtM::None, ImtT::None, -1, None,
        );
        while let Some(mut t) = r {
            if let Some(p) = token_prev(kids, t, true, false) {
                if kids[p].is_group() {
                    group_tokens(kids, GroupKind::TokenList, p, t, true);
                    t = p;
                }
            }
            r = token_next_by(
                text, kids, ImtI::One(GroupKind::Comment), ImtM::None, ImtT::None,
                t as isize, None,
            );
        }
        Ok(())
    })
}

fn group_values(text: &str, kids: &mut Vec<Node>) -> GRes {
    let mut r = token_next_by(
        text, kids, ImtI::None,
        ImtM::One(TType::TOKEN_KEYWORD, MatchVal::One("VALUES")), ImtT::None, -1, None,
    );
    while let Some(t) = r {
        let start_idx = t;
        let mut end_idx: Option<usize> = None;
        let mut cur = token_next(kids, Some(t), true, false);
        while let Some(c) = cur {
            if matches!(&kids[c], Node::Grp { kind: GroupKind::Parenthesis, .. }) {
                end_idx = Some(c);
            }
            cur = token_next(kids, Some(c), true, false);
        }
        if let Some(e) = end_idx {
            group_tokens(kids, GroupKind::Values, start_idx, e, true);
        }
        r = token_next(kids, Some(t), true, false);
    }
    Ok(())
}

/// Full pass over one statement's flat tokens (mirrors `grouping.group`).
pub fn group_stmt(text: &str, kids: &mut Vec<Node>) -> GRes {
    group_comments(text, kids)?;
    group_brackets(text, kids)?;
    group_parenthesis(text, kids)?;
    group_case(text, kids)?;
    group_if(text, kids)?;
    group_for(text, kids)?;
    group_begin(text, kids)?;
    group_over(text, kids)?;
    group_functions(text, kids)?;
    group_where(text, kids)?;
    group_period(text, kids)?;
    group_arrays(text, kids)?;
    group_identifier(text, kids)?;
    group_order(text, kids)?;
    group_typecasts(text, kids)?;
    group_tzcasts(text, kids)?;
    group_typed_literal(text, kids)?;
    group_operator(text, kids)?;
    group_comparison(text, kids)?;
    group_as(text, kids)?;
    group_aliased(text, kids)?;
    group_assignment(text, kids)?;
    align_comments(text, kids)?;
    group_identifier_list(text, kids)?;
    group_values(text, kids)?;
    Ok(())
}

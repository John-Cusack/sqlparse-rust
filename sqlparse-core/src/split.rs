//! Statement splitter mirroring `engine/statement_splitter.py`.
//!
//! Operates on lexer token indices; statements are `Vec<RawTok>` slices.
//! Exact-type checks (`is`) are `==`; keyword-membership (`not in T.Keyword`)
//! is `is_in`; `EOS_TTYPE`/whitespace lists are exact-equality lists.

use crate::lexer::RawTok;
use crate::tables_gen::TType;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Block {
    For,
    While,
    Loop,
    If,
    Case,
    Begin,
    Declare,
}

// (Block variants compare by value; no name table needed.)

pub struct Splitter {
    block_stack: Vec<Block>,
    parenthesis_level: i64,
    unconfirmed_start: Option<Block>,
    is_create: bool,
    seen_begin: bool,
    consume_ws: bool,
    tokens: Vec<RawTok>,
    level: i64,
}

impl Splitter {
    pub fn new() -> Splitter {
        Splitter {
            block_stack: Vec::new(),
            parenthesis_level: 0,
            unconfirmed_start: None,
            is_create: false,
            seen_begin: false,
            consume_ws: false,
            tokens: Vec::new(),
            level: 0,
        }
    }

    fn reset(&mut self) {
        self.block_stack.clear();
        self.parenthesis_level = 0;
        self.unconfirmed_start = None;
        self.is_create = false;
        self.seen_begin = false;
        self.consume_ws = false;
        self.tokens.clear();
        self.level = 0;
    }

    fn handle_nested_block(&mut self, unified: &str) -> Option<i64> {
        if unified == "FOR" {
            self.unconfirmed_start = Some(Block::For);
            return Some(0);
        }
        if unified == "WHILE" {
            self.unconfirmed_start = Some(Block::While);
            return Some(0);
        }
        if unified == "LOOP" || unified == "DO" {
            if matches!(self.unconfirmed_start, Some(Block::For | Block::While)) {
                self.block_stack.push(self.unconfirmed_start.unwrap());
                self.unconfirmed_start = None;
                return Some(1);
            }
            if unified == "LOOP" {
                self.block_stack.push(Block::Loop);
                return Some(1);
            }
        }
        if unified == "IF" || unified == "CASE" {
            self.block_stack.push(if unified == "IF" { Block::If } else { Block::Case });
            return Some(1);
        }
        None
    }

    fn handle_closing_keyword(&mut self, unified: &str) -> i64 {
        let top = self.block_stack.last().copied();
        match unified {
            "END IF" if top == Some(Block::If) => {
                self.block_stack.pop();
                -1
            }
            "END FOR" if top == Some(Block::For) => {
                self.block_stack.pop();
                -1
            }
            "END WHILE" if top == Some(Block::While) => {
                self.block_stack.pop();
                -1
            }
            "END LOOP"
                if matches!(top, Some(Block::Loop | Block::For | Block::While)) =>
            {
                self.block_stack.pop();
                -1
            }
            "END CASE" if top == Some(Block::Case) => {
                self.block_stack.pop();
                -1
            }
            "END" => {
                if !self.block_stack.is_empty() {
                    self.block_stack.pop();
                }
                -1
            }
            _ if unified == "END IF"
                || unified == "END FOR"
                || unified == "END WHILE"
                || unified == "END LOOP"
                || unified == "END CASE" =>
            {
                0
            }
            _ => 0,
        }
    }

    fn change_splitlevel(&mut self, text: &str, ttype: TType, tok: &RawTok) -> i64 {
        let value = &text[tok.start as usize..tok.end as usize];
        if ttype == TType::TOKEN_PUNCTUATION && value == ";" {
            self.unconfirmed_start = None;
            if self.seen_begin {
                self.seen_begin = false;
                if self.block_stack.last() == Some(&Block::Begin) {
                    self.block_stack.pop();
                    return -1;
                }
            }
            return 0;
        }
        if ttype == TType::TOKEN_PUNCTUATION && value == "(" {
            self.parenthesis_level += 1;
            return 1;
        } else if ttype == TType::TOKEN_PUNCTUATION && value == ")" {
            self.parenthesis_level = (self.parenthesis_level - 1).max(0);
            return -1;
        } else if !ttype.is_in(TType::TOKEN_KEYWORD) {
            return 0;
        }
        let unified = value.to_uppercase();
        let unified = unified.as_str();
        if ttype == TType::TOKEN_KEYWORD_DDL && unified.starts_with("CREATE") {
            self.is_create = true;
            return 0;
        }
        if unified == "DECLARE" && self.is_create && self.block_stack.is_empty() {
            self.block_stack.push(Block::Declare);
            return 1;
        }
        if unified == "BEGIN" {
            self.seen_begin = true;
            if self.block_stack.last() == Some(&Block::Declare) {
                self.block_stack.pop();
                self.block_stack.push(Block::Begin);
                return 0;
            } else {
                self.block_stack.push(Block::Begin);
                return 1;
            }
        }
        if self.seen_begin
            && (ttype == TType::TOKEN_KEYWORD || ttype == TType::TOKEN_NAME)
            && matches!(
                unified,
                "TRANSACTION"
                    | "WORK"
                    | "TRAN"
                    | "DISTRIBUTED"
                    | "DEFERRED"
                    | "IMMEDIATE"
                    | "EXCLUSIVE"
            )
        {
            self.seen_begin = false;
            if self.block_stack.last() == Some(&Block::Begin) {
                self.block_stack.pop();
                return -1;
            }
            return 0;
        }
        if self.block_stack.iter().any(|b| *b == Block::Begin) {
            if let Some(res) = self.handle_nested_block(unified) {
                return res;
            }
        }
        self.handle_closing_keyword(unified)
    }

    fn in_begin(&self) -> bool {
        self.block_stack.iter().any(|b| *b == Block::Begin)
    }

    /// Split lexed tokens into statements (each a token vec).
    pub fn split(&mut self, text: &str, toks: &[RawTok]) -> Vec<Vec<RawTok>> {
        let mut out: Vec<Vec<RawTok>> = Vec::new();
        for tok in toks {
            let ttype = tok.ttype;
            let value = &text[tok.start as usize..tok.end as usize];
            if self.consume_ws
                && ttype != TType::TOKEN_TEXT_WHITESPACE
                && ttype != TType::TOKEN_COMMENT_SINGLE
            {
                out.push(std::mem::take(&mut self.tokens));
                self.reset();
            }
            self.level += self.change_splitlevel(text, ttype, tok);
            self.tokens.push(*tok);
            if ttype == TType::TOKEN_PUNCTUATION && value == ";" {
                self.seen_begin = false;
                if self.level <= 0 && !self.in_begin() {
                    self.consume_ws = true;
                }
            } else if ttype == TType::TOKEN_KEYWORD
                && value.split_whitespace().next() == Some("GO")
            {
                self.consume_ws = true;
            } else if !matches!(
                ttype,
                TType::TOKEN_TEXT_WHITESPACE
                    | TType::TOKEN_TEXT_WHITESPACE_NEWLINE
                    | TType::TOKEN_COMMENT_SINGLE
                    | TType::TOKEN_COMMENT_MULTILINE
            ) && !(ttype == TType::TOKEN_KEYWORD && value.to_uppercase() == "BEGIN")
            {
                // Note: upstream resets `_seen_begin` for every non-ws,
                // non-comment token except `BEGIN` itself (which just set it).
                self.seen_begin = false;
            }
        }
        if !self.tokens.is_empty()
            && !self
                .tokens
                .iter()
                .all(|t| t.ttype.is_in(TType::TOKEN_TEXT_WHITESPACE))
        {
            out.push(std::mem::take(&mut self.tokens));
        }
        out
    }
}

/// Strip trailing whitespace/semicolons (mirrors
/// `StripTrailingSemicolonFilter` for `split(strip_semicolon=True)`).
pub fn strip_trailing_semicolon(toks: &mut Vec<RawTok>, text: &str) {
    while let Some(t) = toks.last() {
        let v = &text[t.start as usize..t.end as usize];
        if t.ttype.is_in(TType::TOKEN_TEXT_WHITESPACE) || v == ";" {
            toks.pop();
        } else {
            break;
        }
    }
}

// end of file

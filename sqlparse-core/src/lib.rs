//! `sqlparse-core`: hot-unit port of andialbrecht/sqlparse 0.6.0.
//!
//! Lexer (default configuration) + delimited-span pre-scan + statement
//! splitter + grouping engine + format/split/tokenize string paths. No
//! Python dependency; the whole SQL string crosses once in, results cross
//! once out. See docs in the rustsmith evidence PR; upstream attribution
//! in NOTICE.

pub mod tables_gen;

pub use tables_gen::TType;

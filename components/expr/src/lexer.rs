//! Tokenize an expression: integers, + - * / % ** , comparisons < <= > >= == !=,
//! logical && || !, and parens. Whitespace ignored.
#[derive(Clone, Copy, PartialEq)]
pub enum Tok {
    Num(i64),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    Not,
    LParen,
    RParen,
}
pub fn lex(s: &str) -> Vec<Tok> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let mut n: i64 = 0;
            while i < b.len() && b[i].is_ascii_digit() {
                n = n * 10 + (b[i] as i64 - '0' as i64);
                i += 1;
            }
            out.push(Tok::Num(n));
            continue;
        }
        let next = if i + 1 < b.len() { b[i + 1] } else { '\0' };
        let (t, len) = match (c, next) {
            ('*', '*') => (Tok::Pow, 2),
            ('<', '=') => (Tok::Le, 2),
            ('>', '=') => (Tok::Ge, 2),
            ('=', '=') => (Tok::Eq, 2),
            ('!', '=') => (Tok::Ne, 2),
            ('&', '&') => (Tok::And, 2),
            ('|', '|') => (Tok::Or, 2),
            ('+', _) => (Tok::Plus, 1),
            ('-', _) => (Tok::Minus, 1),
            ('*', _) => (Tok::Star, 1),
            ('/', _) => (Tok::Slash, 1),
            ('%', _) => (Tok::Percent, 1),
            ('<', _) => (Tok::Lt, 1),
            ('>', _) => (Tok::Gt, 1),
            ('!', _) => (Tok::Not, 1),
            ('(', _) => (Tok::LParen, 1),
            (')', _) => (Tok::RParen, 1),
            _ => panic!("unexpected character"),
        };
        out.push(t);
        i += len;
    }
    out
}

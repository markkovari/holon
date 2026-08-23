//! Tokenize an arithmetic expression.
#[derive(Clone, Copy, PartialEq)]
pub enum Tok {
    Num(i64),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
}

pub fn lex(s: &str) -> Vec<Tok> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_digit() {
            let mut n: i64 = 0;
            while i < b.len() && b[i].is_ascii_digit() {
                n = n * 10 + (b[i] as i64 - '0' as i64);
                i += 1;
            }
            out.push(Tok::Num(n));
            continue;
        }
        match c {
            '+' => out.push(Tok::Plus),
            '-' => out.push(Tok::Minus),
            '*' => out.push(Tok::Star),
            '/' => out.push(Tok::Slash),
            '(' => out.push(Tok::LParen),
            ')' => out.push(Tok::RParen),
            _ => {}
        }
        i += 1;
    }
    out
}

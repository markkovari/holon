//! Evaluate the token stream with correct precedence, left-associativity,
//! parentheses, and unary minus. Integer division.
use super::lexer::{lex, Tok};

pub fn eval(s: &str) -> i64 {
    let toks = lex(s);
    let mut p = 0usize;
    expr(&toks, &mut p)
}

fn expr(t: &[Tok], p: &mut usize) -> i64 {
    let mut v = term(t, p);
    loop {
        match t.get(*p) {
            Some(Tok::Plus) => { *p += 1; v += term(t, p); }
            Some(Tok::Minus) => { *p += 1; v -= term(t, p); }
            _ => return v,
        }
    }
}

fn term(t: &[Tok], p: &mut usize) -> i64 {
    let mut v = unary(t, p);
    loop {
        match t.get(*p) {
            Some(Tok::Star) => { *p += 1; v *= unary(t, p); }
            Some(Tok::Slash) => { *p += 1; v /= unary(t, p); }
            _ => return v,
        }
    }
}

fn unary(t: &[Tok], p: &mut usize) -> i64 {
    match t.get(*p) {
        Some(Tok::Minus) => { *p += 1; -unary(t, p) }
        Some(Tok::Plus) => { *p += 1; unary(t, p) }
        _ => atom(t, p),
    }
}

fn atom(t: &[Tok], p: &mut usize) -> i64 {
    match t.get(*p) {
        Some(Tok::Num(n)) => { let n = *n; *p += 1; n }
        Some(Tok::LParen) => {
            *p += 1;
            let v = expr(t, p);
            if let Some(Tok::RParen) = t.get(*p) { *p += 1; }
            v
        }
        _ => 0,
    }
}
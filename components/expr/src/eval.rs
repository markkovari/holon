//! Evaluate with correct precedence and associativity, tightest first:
//!   ** (RIGHT-assoc) > unary (- !) > * / % > + - > comparisons > && > ||
//! Bools are 0/1; truthiness is "nonzero"; / and % are integer.
use super::lexer::{lex, Tok};

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<Tok> {
        self.t.get(self.i).copied()
    }
    fn eat(&mut self, k: Tok) -> bool {
        if self.peek() == Some(k) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn or(&mut self) -> i64 {
        let mut a = self.and();
        while self.eat(Tok::Or) {
            let b = self.and();
            a = ((a != 0) || (b != 0)) as i64;
        }
        a
    }
    fn and(&mut self) -> i64 {
        let mut a = self.cmp();
        while self.eat(Tok::And) {
            let b = self.cmp();
            a = ((a != 0) && (b != 0)) as i64;
        }
        a
    }
    fn cmp(&mut self) -> i64 {
        let mut a = self.add();
        loop {
            let op = match self.peek() {
                Some(t @ (Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge | Tok::Eq | Tok::Ne)) => t,
                _ => return a,
            };
            self.i += 1;
            let b = self.add();
            a = match op {
                Tok::Lt => a < b,
                Tok::Le => a <= b,
                Tok::Gt => a > b,
                Tok::Ge => a >= b,
                Tok::Eq => a == b,
                _ => a != b,
            } as i64;
        }
    }
    fn add(&mut self) -> i64 {
        let mut a = self.mul();
        loop {
            if self.eat(Tok::Plus) {
                a += self.mul();
            } else if self.eat(Tok::Minus) {
                a -= self.mul();
            } else {
                return a;
            }
        }
    }
    fn mul(&mut self) -> i64 {
        let mut a = self.unary();
        loop {
            if self.eat(Tok::Star) {
                a *= self.unary();
            } else if self.eat(Tok::Slash) {
                a /= self.unary();
            } else if self.eat(Tok::Percent) {
                a %= self.unary();
            } else {
                return a;
            }
        }
    }
    fn unary(&mut self) -> i64 {
        if self.eat(Tok::Minus) {
            -self.unary()
        } else if self.eat(Tok::Not) {
            (self.unary() == 0) as i64
        } else if self.eat(Tok::Plus) {
            self.unary()
        } else {
            self.power()
        }
    }
    fn power(&mut self) -> i64 {
        let base = self.atom();
        if self.eat(Tok::Pow) {
            let e = self.unary();
            let mut r: i64 = 1;
            if e < 0 {
                return 0;
            }
            for _ in 0..e {
                r *= base;
            }
            r
        } else {
            base
        }
    }
    fn atom(&mut self) -> i64 {
        match self.peek() {
            Some(Tok::Num(n)) => {
                self.i += 1;
                n
            }
            Some(Tok::LParen) => {
                self.i += 1;
                let v = self.or();
                if !self.eat(Tok::RParen) {
                    panic!("expected )");
                }
                v
            }
            _ => panic!("unexpected token"),
        }
    }
}

pub fn eval(s: &str) -> i64 {
    let mut p = P { t: lex(s), i: 0 };
    let v = p.or();
    if p.i != p.t.len() {
        panic!("trailing input");
    }
    v
}

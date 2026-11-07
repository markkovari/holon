// Implementation of demo:expr@0.1.0 language interface in MoonBit

#derive(Eq)
enum Tok {
  Num(Int64)
  Plus
  Minus
  Star
  Slash
  Percent
  Pow
  Lt
  Le
  Gt
  Ge
  Eq
  Ne
  And
  Or
  Not
  LParen
  RParen
}

fn is_ws(c : Char) -> Bool {
  c == ' ' || c == '\t' || c == '\n' || c == '\r'
}

fn lex(s : String) -> Array[Tok] {
  let b : Array[Char] = []
  for c in s {
    b.push(c)
  }
  let mut i = 0
  let out : Array[Tok] = []
  let zero_code = '0'.to_int()
  while i < b.length() {
    let c = b[i]
    if is_ws(c) {
      i = i + 1
      continue
    }
    if c >= '0' && c <= '9' {
      let mut n = 0L
      while i < b.length() && b[i] >= '0' && b[i] <= '9' {
        let digit = (b[i].to_int() - zero_code).to_int64()
        n = n * 10L + digit
        i = i + 1
      }
      out.push(Tok::Num(n))
      continue
    }
    let next = if i + 1 < b.length() { b[i + 1] } else { '\u0000' }
    let (t, len) = match (c, next) {
      ('*', '*') => (Tok::Pow, 2)
      ('<', '=') => (Tok::Le, 2)
      ('>', '=') => (Tok::Ge, 2)
      ('=', '=') => (Tok::Eq, 2)
      ('!', '=') => (Tok::Ne, 2)
      ('&', '&') => (Tok::And, 2)
      ('|', '|') => (Tok::Or, 2)
      ('+', _) => (Tok::Plus, 1)
      ('-', _) => (Tok::Minus, 1)
      ('*', _) => (Tok::Star, 1)
      ('/', _) => (Tok::Slash, 1)
      ('%', _) => (Tok::Percent, 1)
      ('<', _) => (Tok::Lt, 1)
      ('>', _) => (Tok::Gt, 1)
      ('!', _) => (Tok::Not, 1)
      ('(', _) => (Tok::LParen, 1)
      (')', _) => (Tok::RParen, 1)
      _ => abort("unexpected character")
    }
    out.push(t)
    i = i + len
  }
  out
}

priv struct Parser {
  t : Array[Tok]
  mut i : Int
}

fn Parser::new(t : Array[Tok]) -> Parser {
  { t, i: 0 }
}

fn Parser::peek(self : Parser) -> Tok? {
  if self.i < self.t.length() {
    Some(self.t[self.i])
  } else {
    None
  }
}

fn Parser::eat(self : Parser, k : Tok) -> Bool {
  match self.peek() {
    Some(t) =>
      if t == k {
        self.i = self.i + 1
        true
      } else {
        false
      }
    None => false
  }
}

fn Parser::eval_or(self : Parser) -> Int64 {
  let mut a = self.eval_and()
  while self.eat(Tok::Or) {
    let b = self.eval_and()
    a = if a != 0L || b != 0L { 1L } else { 0L }
  }
  a
}

fn Parser::eval_and(self : Parser) -> Int64 {
  let mut a = self.eval_cmp()
  while self.eat(Tok::And) {
    let b = self.eval_cmp()
    a = if a != 0L && b != 0L { 1L } else { 0L }
  }
  a
}

fn Parser::eval_cmp(self : Parser) -> Int64 {
  let mut a = self.eval_add()
  while true {
    let op = match self.peek() {
      Some(Tok::Lt) => Tok::Lt
      Some(Tok::Le) => Tok::Le
      Some(Tok::Gt) => Tok::Gt
      Some(Tok::Ge) => Tok::Ge
      Some(Tok::Eq) => Tok::Eq
      Some(Tok::Ne) => Tok::Ne
      _ => return a
    }
    self.i = self.i + 1
    let b = self.eval_add()
    let cond = match op {
      Tok::Lt => a < b
      Tok::Le => a <= b
      Tok::Gt => a > b
      Tok::Ge => a >= b
      Tok::Eq => a == b
      _ => a != b
    }
    a = if cond { 1L } else { 0L }
  }
  a
}

fn Parser::eval_add(self : Parser) -> Int64 {
  let mut a = self.eval_mul()
  while true {
    if self.eat(Tok::Plus) {
      a = a + self.eval_mul()
    } else if self.eat(Tok::Minus) {
      a = a - self.eval_mul()
    } else {
      return a
    }
  }
  a
}

fn Parser::eval_mul(self : Parser) -> Int64 {
  let mut a = self.eval_unary()
  while true {
    if self.eat(Tok::Star) {
      a = a * self.eval_unary()
    } else if self.eat(Tok::Slash) {
      a = a / self.eval_unary()
    } else if self.eat(Tok::Percent) {
      a = a % self.eval_unary()
    } else {
      return a
    }
  }
  a
}

fn Parser::eval_unary(self : Parser) -> Int64 {
  if self.eat(Tok::Minus) {
    -self.eval_unary()
  } else if self.eat(Tok::Not) {
    if self.eval_unary() == 0L {
      1L
    } else {
      0L
    }
  } else if self.eat(Tok::Plus) {
    self.eval_unary()
  } else {
    self.eval_power()
  }
}

fn Parser::eval_power(self : Parser) -> Int64 {
  let base = self.eval_atom()
  if self.eat(Tok::Pow) {
    let e = self.eval_unary()
    if e < 0L {
      return 0L
    }
    let mut r = 1L
    let mut count = e
    while count > 0L {
      r = r * base
      count = count - 1L
    }
    r
  } else {
    base
  }
}

fn Parser::eval_atom(self : Parser) -> Int64 {
  match self.peek() {
    Some(Tok::Num(n)) => {
      self.i = self.i + 1
      n
    }
    Some(Tok::LParen) => {
      self.i = self.i + 1
      let v = self.eval_or()
      if !self.eat(Tok::RParen) {
        abort("expected )")
      }
      v
    }
    _ => abort("unexpected token")
  }
}

///|
pub fn eval(src : String) -> Int64 {
  let p = Parser::new(lex(src))
  let v = p.eval_or()
  if p.i != p.t.length() {
    abort("trailing input")
  }
  v
}

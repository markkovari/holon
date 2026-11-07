// Implementation of demo:calc@0.1.0 arith interface in MoonBit

#derive(Eq)
enum Tok {
  Num(Int64)
  Plus
  Minus
  Star
  Slash
  LParen
  RParen
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
    match c {
      '+' => out.push(Tok::Plus)
      '-' => out.push(Tok::Minus)
      '*' => out.push(Tok::Star)
      '/' => out.push(Tok::Slash)
      '(' => out.push(Tok::LParen)
      ')' => out.push(Tok::RParen)
      _ => ()
    }
    i = i + 1
  }
  out
}

priv struct Parser {
  toks : Array[Tok]
  mut pos : Int
}

fn Parser::new(toks : Array[Tok]) -> Parser {
  { toks, pos: 0 }
}

fn Parser::peek(self : Parser) -> Tok? {
  if self.pos < self.toks.length() {
    Some(self.toks[self.pos])
  } else {
    None
  }
}

fn Parser::expr(self : Parser) -> Int64 {
  let mut v = self.term()
  while true {
    match self.peek() {
      Some(Tok::Plus) => {
        self.pos = self.pos + 1
        v = v + self.term()
      }
      Some(Tok::Minus) => {
        self.pos = self.pos + 1
        v = v - self.term()
      }
      _ => return v
    }
  }
  v
}

fn Parser::term(self : Parser) -> Int64 {
  let mut v = self.unary()
  while true {
    match self.peek() {
      Some(Tok::Star) => {
        self.pos = self.pos + 1
        v = v * self.unary()
      }
      Some(Tok::Slash) => {
        self.pos = self.pos + 1
        v = v / self.unary()
      }
      _ => return v
    }
  }
  v
}

fn Parser::unary(self : Parser) -> Int64 {
  match self.peek() {
    Some(Tok::Minus) => {
      self.pos = self.pos + 1
      -self.unary()
    }
    Some(Tok::Plus) => {
      self.pos = self.pos + 1
      self.unary()
    }
    _ => self.atom()
  }
}

fn Parser::atom(self : Parser) -> Int64 {
  match self.peek() {
    Some(Tok::Num(n)) => {
      self.pos = self.pos + 1
      n
    }
    Some(Tok::LParen) => {
      self.pos = self.pos + 1
      let v = self.expr()
      match self.peek() {
        Some(Tok::RParen) => self.pos = self.pos + 1
        _ => ()
      }
      v
    }
    _ => 0L
  }
}

///|
pub fn eval(expr : String) -> Int64 {
  let toks = lex(expr)
  let parser = Parser::new(toks)
  parser.expr()
}

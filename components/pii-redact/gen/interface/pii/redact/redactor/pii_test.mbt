test "luhn accepts real check digits and rejects near misses" {
  assert_true(@redactor.luhn([4, 5, 3, 9, 1, 4, 8, 8, 0, 3, 4, 3, 6, 4, 6, 7]))
  assert_true(@redactor.luhn([7, 9, 9, 2, 7, 3, 9, 8, 7, 1, 3]))
  assert_false(@redactor.luhn([4, 5, 3, 9, 1, 4, 8, 8, 0, 3, 4, 3, 6, 4, 6, 8]))
  assert_false(@redactor.luhn([7, 9, 9, 2, 7, 3, 9, 8, 7, 3, 1]))
}

test "a card is found with or without separators" {
  let opts = @redactor.Options::{ kinds: [@redactor.CREDIT_CARD] }
  assert_eq(@redactor.detect("pay 4539148803436467 now", opts).length(), 1)
  assert_eq(@redactor.detect("4539-1488-0343-6467", opts).length(), 1)
  assert_eq(@redactor.detect("4539 1488 0343 6467", opts).length(), 1)
  assert_eq(@redactor.detect("4539148803436468", opts).length(), 0)
}

test "an ssn does not match inside a longer number" {
  let opts = @redactor.Options::{ kinds: [@redactor.SSN] }
  let found = @redactor.detect("ssn 123-45-6789 ok", opts)
  assert_eq(found.length(), 1)
  assert_eq(found[0].start, 4U)
  assert_eq(found[0].length, 11U)
  assert_eq(@redactor.detect("9123-45-6789", opts).length(), 0)
  assert_eq(@redactor.detect("123-45-67891", opts).length(), 0)
  assert_eq(@redactor.detect("123456789", opts).length(), 0)
}

test "a higher priority kind claims an overlapping span" {
  let opts = @redactor.Options::{ kinds: [] }
  let found = @redactor.detect("call 4539148803436467 today", opts)
  assert_eq(found.length(), 1)
  assert_eq(@redactor.Kind::ordinal(found[0].kind), @redactor.Kind::ordinal(@redactor.CREDIT_CARD))
}

test "findings are sorted by start" {
  let opts = @redactor.Options::{ kinds: [] }
  let found = @redactor.detect("a@b.com then 123-45-6789 then c@d.org", opts)
  assert_true(found.length() >= 2)
  for i = 0; i < found.length() - 1; i = i + 1 {
    assert_true(found[i].start <= found[i + 1].start)
  }
}

test "an email needs a tld" {
  let opts = @redactor.Options::{ kinds: [@redactor.EMAIL] }
  assert_eq(@redactor.detect("mail me at a@b.com ok", opts).length(), 1)
  assert_eq(@redactor.detect("a@localhost", opts).length(), 0)
  assert_eq(@redactor.detect("not an @ sign", opts).length(), 0)
}

test "redact and mask works" {
  let opts = @redactor.Options::{ kinds: [] }
  let red = @redactor.redact("Contact test@example.com or 123-45-6789 at 192.168.1.1", opts)
  assert_eq(red, "Contact [EMAIL] or [SSN] at [IP]")
  let msk = @redactor.mask("Contact test@example.com or 123-45-6789 at 192.168.1.1", opts)
  assert_eq(msk, "Contact t***@e***.com or ***-**-6789 at ***.***.***.1")
}

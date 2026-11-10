// Unit tests for mail-parse.

fn str_to_raw(s : String) -> FixedArray[Byte] {
  let bytes = @utf8.encode(s)
  FixedArray::makei(bytes.length(), fn(i) { bytes[i] })
}

test "simple plain text email" {
  let raw = str_to_raw(
    "From: alice@example.com\r\n" +
    "Subject: Hello World\r\n" +
    "In-Reply-To: <12345@example.com>\r\n" +
    "\r\n" +
    "This is a test email message.\r\n"
  )
  let res = parse(raw)
  match res {
    Ok(email) => {
      assert_eq(email.sender, "alice@example.com")
      assert_eq(email.subject, "Hello World")
      assert_eq(email.in_reply_to, Some("<12345@example.com>"))
      assert_eq(email.text, "This is a test email message.\r\n")
      assert_eq(email.html, None)
    }
    Err(_) => panic()
  }
}

test "multipart alternative with html" {
  let raw = str_to_raw(
    "From: bob@example.com\r\n" +
    "Subject: Multi Part\r\n" +
    "Content-Type: multipart/alternative; boundary=\"boundary42\"\r\n" +
    "\r\n" +
    "--boundary42\r\n" +
    "Content-Type: text/plain\r\n" +
    "\r\n" +
    "Plain body\r\n" +
    "--boundary42\r\n" +
    "Content-Type: text/html\r\n" +
    "\r\n" +
    "<p>Html body</p>\r\n" +
    "--boundary42--\r\n"
  )
  let res = parse(raw)
  match res {
    Ok(email) => {
      assert_eq(email.sender, "bob@example.com")
      assert_eq(email.subject, "Multi Part")
      assert_eq(email.text, "Plain body")
      assert_eq(email.html, Some("<p>Html body</p>"))
    }
    Err(_) => panic()
  }
}

test "base64 body decoding" {
  // "Hello from base64!" base64 is "SGVsbG8gZnJvbSBiYXNlNjQh"
  let raw = str_to_raw(
    "From: carol@example.com\r\n" +
    "Subject: Base64 Test\r\n" +
    "Content-Transfer-Encoding: base64\r\n" +
    "\r\n" +
    "SGVsbG8gZnJvbSBiYXNlNjQh\r\n"
  )
  let res = parse(raw)
  match res {
    Ok(email) => {
      assert_eq(email.sender, "carol@example.com")
      assert_eq(email.text, "Hello from base64!")
    }
    Err(_) => panic()
  }
}

test "quoted printable body decoding" {
  let raw = str_to_raw(
    "From: dave@example.com\r\n" +
    "Subject: QP Test\r\n" +
    "Content-Transfer-Encoding: quoted-printable\r\n" +
    "\r\n" +
    "Hello=20World=\r\n!\r\n"
  )
  let res = parse(raw)
  match res {
    Ok(email) => {
      assert_eq(email.sender, "dave@example.com")
      assert_eq(email.text, "Hello World!\r\n")
    }
    Err(_) => panic()
  }
}

test "mime encoded subject" {
  // =?UTF-8?B?SGVsbG8gV29ybGQ=?= is "Hello World"
  let raw = str_to_raw(
    "From: eve@example.com\r\n" +
    "Subject: =?UTF-8?B?SGVsbG8gV29ybGQ=?=\r\n" +
    "\r\n" +
    "Body\r\n"
  )
  let res = parse(raw)
  match res {
    Ok(email) => {
      assert_eq(email.subject, "Hello World")
    }
    Err(_) => panic()
  }
}

test "folded headers" {
  let raw = str_to_raw(
    "From: frank@example.com\r\n" +
    "Subject: Line one\r\n" +
    " and line two\r\n" +
    "\r\n" +
    "Body\r\n"
  )
  let res = parse(raw)
  match res {
    Ok(email) => {
      assert_eq(email.subject, "Line one and line two")
    }
    Err(_) => panic()
  }
}

test "missing from returns malformed" {
  let raw = str_to_raw(
    "Subject: No Sender\r\n" +
    "\r\n" +
    "Body\r\n"
  )
  let res = parse(raw)
  match res {
    Err(Malformed(msg)) => {
      assert_eq(msg, "Missing From header")
    }
    _ => panic()
  }
}

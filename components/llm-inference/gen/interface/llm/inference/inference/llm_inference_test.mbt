fn default_opts() -> @inference.Options {
  { model: "", temperature: 0U, max_tokens: 0U, stop: [], seed: 0UL }
}

test "empty chat messages returns error" {
  let res = @inference.chat([], default_opts())
  assert_eq(res is Err(InvalidRequest(_)), true)
}

test "basic mock chat echoes user" {
  let msgs : Array[@inference.Message] = [
    { role: SYSTEM, content: "You are a helpful assistant." },
    { role: USER, content: "Hello world" },
  ]
  match @inference.chat(msgs, default_opts()) {
    Ok(c) => {
      assert_eq(c.text, "mock: Hello world")
      assert_eq(c.model, "mock-1")
      assert_eq(c.finish_reason, "stop")
    }
    Err(_) => fail("expected Ok")
  }
}

test "classify with labels" {
  let msgs : Array[@inference.Message] = [
    { role: USER, content: "Classify this text. labels: positive, negative, neutral." },
  ]
  match @inference.chat(msgs, default_opts()) {
    Ok(c) => assert_eq(c.text, "positive")
    Err(_) => fail("expected Ok")
  }
}

test "extract fields" {
  let msgs : Array[@inference.Message] = [
    { role: USER, content: "Extract info. fields: name, email, phone\nMore details." },
  ]
  match @inference.chat(msgs, default_opts()) {
    Ok(c) => assert_eq(c.text, "{\"name\":\"mock-name\",\"email\":\"mock-email\",\"phone\":\"mock-phone\"}")
    Err(_) => fail("expected Ok")
  }
}

test "summarize text" {
  let msgs : Array[@inference.Message] = [
    { role: USER, content: "Summarize this article please: The quick brown fox jumps over the lazy dog." },
  ]
  match @inference.chat(msgs, default_opts()) {
    Ok(c) => assert_eq(c.text.contains("Summary:"), true)
    Err(_) => fail("expected Ok")
  }
}

test "complete and describe" {
  let (m, has_embed) = @inference.describe()
  assert_eq(m, "mock-1")
  assert_eq(has_embed, true)

  match @inference.complete("ping", "sys", default_opts()) {
    Ok(c) => assert_eq(c.text, "mock: ping")
    Err(_) => fail("expected Ok")
  }
}

test "embed vector generation" {
  match @inference.embed("sample text for embedding", default_opts()) {
    Ok(vec) => {
      assert_eq(vec.length(), 8)
      for i = 0; i < 8; i = i + 1 {
        assert_eq(vec[i] >= 0.0, true)
        assert_eq(vec[i] <= 1.0, true)
      }
    }
    Err(_) => fail("expected Ok")
  }
}

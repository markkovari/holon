test "plain lowercase word boundaries split on uppercase" {
  assert_eq(@conv.to_snake("helloWorld"), "hello_world")
  assert_eq(@conv.to_kebab("helloWorld"), "hello-world")
  assert_eq(@conv.to_camel("helloWorld"), "helloWorld")
  assert_eq(@conv.to_pascal("helloWorld"), "HelloWorld")
}

test "snake and kebab round trip into camel and pascal" {
  assert_eq(@conv.to_camel("hello_world"), "helloWorld")
  assert_eq(@conv.to_pascal("hello_world"), "HelloWorld")
  assert_eq(@conv.to_camel("hello-world"), "helloWorld")
  assert_eq(@conv.to_snake("hello-world"), "hello_world")
  assert_eq(@conv.to_kebab("hello_world"), "hello-world")
}

test "an acronym run splits before its last letter not its first" {
  assert_eq(@conv.to_snake("HTTPServer"), "http_server")
  assert_eq(@conv.to_pascal("HTTPServer"), "HttpServer")
  assert_eq(@conv.to_camel("HTTPServer"), "httpServer")
}

test "multiple acronyms each split correctly" {
  assert_eq(@conv.to_snake("XMLHttpRequest"), "xml_http_request")
  assert_eq(@conv.to_pascal("XMLHttpRequest"), "XmlHttpRequest")
}

test "a digit boundary is its own word either direction" {
  assert_eq(@conv.to_snake("Sensor2Value"), "sensor_2_value")
  assert_eq(@conv.to_snake("2Sensors"), "2_sensors")
  assert_eq(@conv.to_pascal("sensor2value"), "Sensor2Value")
}

test "consecutive separators collapse and edges produce no empty word" {
  assert_eq(@conv.to_snake("foo__bar"), "foo_bar")
  assert_eq(@conv.to_snake("_foo_"), "foo")
  assert_eq(@conv.to_snake("-foo-"), "foo")
  assert_eq(@conv.to_kebab("__foo__bar__"), "foo-bar")
}

test "an empty string is empty in every convention" {
  assert_eq(@conv.to_snake(""), "")
  assert_eq(@conv.to_kebab(""), "")
  assert_eq(@conv.to_camel(""), "")
  assert_eq(@conv.to_pascal(""), "")
}

test "a single word is stable across conventions" {
  assert_eq(@conv.to_snake("word"), "word")
  assert_eq(@conv.to_camel("word"), "word")
  assert_eq(@conv.to_pascal("word"), "Word")
  assert_eq(@conv.to_kebab("WORD"), "word")
}

test "an all uppercase run with no following lowercase is one word" {
  assert_eq(@conv.to_snake("ALLCAPS"), "allcaps")
  assert_eq(@conv.to_pascal("ALLCAPS"), "Allcaps")
}

test "already correct case round trips unchanged" {
  assert_eq(@conv.to_snake("already_snake_case"), "already_snake_case")
  assert_eq(@conv.to_kebab("already-kebab-case"), "already-kebab-case")
  assert_eq(@conv.to_camel("alreadyCamelCase"), "alreadyCamelCase")
  assert_eq(@conv.to_pascal("AlreadyPascalCase"), "AlreadyPascalCase")
}

// The specification for `case-conv`. Not writable by the goal that implements
// `src/lib.rs` — see `.comp/goals/case-conv.toml`.

#[test]
fn plain_lowercase_word_boundaries_split_on_uppercase() {
    assert_eq!(case_conv::to_snake("helloWorld"), "hello_world");
    assert_eq!(case_conv::to_kebab("helloWorld"), "hello-world");
    assert_eq!(case_conv::to_camel("helloWorld"), "helloWorld");
    assert_eq!(case_conv::to_pascal("helloWorld"), "HelloWorld");
}

#[test]
fn snake_and_kebab_round_trip_into_camel_and_pascal() {
    assert_eq!(case_conv::to_camel("hello_world"), "helloWorld");
    assert_eq!(case_conv::to_pascal("hello_world"), "HelloWorld");
    assert_eq!(case_conv::to_camel("hello-world"), "helloWorld");
    assert_eq!(case_conv::to_snake("hello-world"), "hello_world");
    assert_eq!(case_conv::to_kebab("hello_world"), "hello-world");
}

#[test]
fn an_acronym_run_splits_before_its_last_letter_not_its_first() {
    // HTTPServer -> "HTTP", "Server" — not "H", "TTPServer" and not "HTTPS", "erver".
    assert_eq!(case_conv::to_snake("HTTPServer"), "http_server");
    assert_eq!(case_conv::to_pascal("HTTPServer"), "HttpServer");
    assert_eq!(case_conv::to_camel("HTTPServer"), "httpServer");
}

#[test]
fn multiple_acronyms_each_split_correctly() {
    assert_eq!(case_conv::to_snake("XMLHttpRequest"), "xml_http_request");
    assert_eq!(case_conv::to_pascal("XMLHttpRequest"), "XmlHttpRequest");
}

#[test]
fn a_digit_boundary_is_its_own_word_either_direction() {
    assert_eq!(case_conv::to_snake("Sensor2Value"), "sensor_2_value");
    assert_eq!(case_conv::to_snake("2Sensors"), "2_sensors");
    assert_eq!(case_conv::to_pascal("sensor2value"), "Sensor2Value");
}

#[test]
fn consecutive_separators_collapse_and_edges_produce_no_empty_word() {
    assert_eq!(case_conv::to_snake("foo__bar"), "foo_bar");
    assert_eq!(case_conv::to_snake("_foo_"), "foo");
    assert_eq!(case_conv::to_snake("-foo-"), "foo");
    assert_eq!(case_conv::to_kebab("__foo__bar__"), "foo-bar");
}

#[test]
fn an_empty_string_is_empty_in_every_convention() {
    assert_eq!(case_conv::to_snake(""), "");
    assert_eq!(case_conv::to_kebab(""), "");
    assert_eq!(case_conv::to_camel(""), "");
    assert_eq!(case_conv::to_pascal(""), "");
}

#[test]
fn a_single_word_is_stable_across_conventions() {
    assert_eq!(case_conv::to_snake("word"), "word");
    assert_eq!(case_conv::to_camel("word"), "word");
    assert_eq!(case_conv::to_pascal("word"), "Word");
    assert_eq!(case_conv::to_kebab("WORD"), "word");
}

#[test]
fn an_all_uppercase_run_with_no_following_lowercase_is_one_word() {
    assert_eq!(case_conv::to_snake("ALLCAPS"), "allcaps");
    assert_eq!(case_conv::to_pascal("ALLCAPS"), "Allcaps");
}

#[test]
fn already_correct_case_round_trips_unchanged() {
    assert_eq!(case_conv::to_snake("already_snake_case"), "already_snake_case");
    assert_eq!(case_conv::to_kebab("already-kebab-case"), "already-kebab-case");
    assert_eq!(case_conv::to_camel("alreadyCamelCase"), "alreadyCamelCase");
    assert_eq!(case_conv::to_pascal("AlreadyPascalCase"), "AlreadyPascalCase");
}

//! The held-out specification, corrected against cucumber/gherkin's `testdata/` (see `corpus.rs`).

use gherkin_validate::{validate, Kind, Problem, Severity};

fn kinds(source: &str) -> Vec<Kind> {
    validate(source).into_iter().map(|p| p.kind).collect()
}

fn at(source: &str) -> Vec<(u32, u32)> {
    validate(source).into_iter().map(|p| (p.line, p.column)).collect()
}

/// Must be clean, or every other assertion measures a parser that cannot read Gherkin.
#[test]
fn a_real_feature_file_is_valid() {
    let src = r#"# language: en
@billing @slow
Feature: Refunds
  As a customer I want my money back
  so that I stop being a customer.

  Background:
    Given the shop is open
    And I am signed in

  @happy
  Scenario: A full refund
    Given I bought a "Widget" for 20.00
    When I request a refund
    Then I am refunded 20.00
    And the order reads:
      | field  | value    |
      | status | refunded |
      | total  | 0.00     |

  Scenario Outline: Partial refunds
    Given I bought a <item> for <price>
    When I return <count> of them
    Then I am refunded <refund>

    Examples: whole units
      | item   | price | count | refund |
      | Widget | 20.00 | 1     | 20.00  |
      | Gizmo  | 5.00  | 2     | 10.00  |

  Scenario: A refund explains itself
    When I request a refund
    Then the receipt says:
      """
      Given you asked within 30 days
      When we processed it
      """
"#;
    assert_eq!(validate(src), Vec::<Problem>::new(), "this file is valid Gherkin");
}

#[test]
fn nothing_to_read_is_not_a_problem() {
    assert!(validate("").is_empty(), "an empty file");
    assert!(validate("\n\n  \n").is_empty(), "whitespace only");
    assert!(validate("# just a note\n# and another\n").is_empty(), "comments only");
}

#[test]
fn content_before_the_feature_is_refused_but_tags_and_comments_are_not() {
    let bad = "Some notes I pasted here\nFeature: Thing\n  Scenario: S\n    Given a\n";
    assert_eq!(kinds(bad), vec![Kind::ContentBeforeFeature("Some notes I pasted here".into())]);

    let good = "# a note\n@tagged\n# language: en\nFeature: Thing\n  Scenario: S\n    Given a\n";
    assert!(validate(good).is_empty(), "{:?}", validate(good));
}

#[test]
fn a_file_with_content_but_no_feature() {
    let src = "Scenario: orphaned\n  Given a thing\n";
    assert!(kinds(src).contains(&Kind::NoFeature), "{:?}", kinds(src));
}

#[test]
fn two_features_in_one_file() {
    let src =
        "Feature: One\n  Scenario: S\n    Given a\nFeature: Two\n  Scenario: T\n    Given b\n";
    assert_eq!(kinds(src), vec![Kind::MultipleFeatures]);
    assert_eq!(at(src)[0].0, 4, "the SECOND feature is the problem, not the first");
}

#[test]
fn a_step_with_no_scenario_above_it() {
    let src = "Feature: Thing\n  Given a thing\n";
    assert_eq!(kinds(src), vec![Kind::StepOutsideScenario]);
}

/// Cucumber parses a leading `And`, so this only warns.
#[test]
fn a_continuation_with_nothing_to_continue() {
    for kw in ["And", "But"] {
        let src = format!("Feature: T\n  Scenario: S\n    {kw} a thing\n    Then b\n");
        assert_eq!(
            kinds(&src),
            vec![Kind::ContinuationWithoutAStep(kw.to_string())],
            "{kw} as the first step"
        );
    }
    let ok = "Feature: T\n  Scenario: S\n    Given a\n    And b\n    But not c\n    * d\n";
    assert!(validate(ok).is_empty(), "{:?}", validate(ok));

    // `*` is not a continuation (`good/star-keywords.feature`).
    let stars =
        "Feature: Stars\n  Scenario: Beautiful tonight\n    * Betelgeuse\n    * Alpha Centauri\n";
    assert!(validate(stars).is_empty(), "a bullet needs nothing above it: {:?}", validate(stars));
}

#[test]
fn an_outline_with_no_examples_can_never_run() {
    let src = "Feature: T\n  Scenario Outline: Partial\n    Given <a>\n";
    let k = kinds(src);
    assert!(
        k.contains(&Kind::OutlineWithoutExamples("Partial".into())),
        "should name the outline: {k:?}"
    );
}

#[test]
fn examples_promote_a_plain_scenario_to_an_outline() {
    let promoted = "Feature: T\n  Scenario: S\n    Given the <what>\n\n    Examples:\n      | what |\n      | this |\n";
    assert!(validate(promoted).is_empty(), "{:?}", validate(promoted));

    let typo = "Feature: T\n  Scenario: S\n    Given the <waht>\n\n    Examples:\n      | what |\n      | this |\n";
    assert_eq!(kinds(typo), vec![Kind::UnknownPlaceholder("waht".into())]);
}

#[test]
fn examples_with_no_scenario_at_all() {
    let src = "Feature: T\n  Examples:\n    | x |\n    | 1 |\n";
    assert_eq!(kinds(src), vec![Kind::ExamplesOutsideAScenario]);
}

#[test]
fn a_scenario_with_no_steps() {
    let src = "Feature: T\n  Scenario: Empty one\n  Scenario: S\n    Given a\n";
    assert_eq!(kinds(src), vec![Kind::EmptyScenario("Empty one".into())]);

    let bg = "Feature: T\n  Background:\n  Scenario: S\n    Given a\n";
    assert_eq!(kinds(bg), vec![Kind::EmptyScenario("Background".into())]);
}

#[test]
fn an_examples_row_of_the_wrong_width() {
    let src = "\
Feature: T
  Scenario Outline: O
    Given <a> and <b>

    Examples:
      | a | b |
      | 1 | 2 |
      | 3 |
";
    assert_eq!(kinds(src), vec![Kind::RowWidth { expected: 2, found: 1 }]);
    assert_eq!(at(src)[0].0, 8, "the offending row's line");
}

#[test]
fn a_data_table_row_of_the_wrong_width() {
    let src = "\
Feature: T
  Scenario: S
    Given the table:
      | a | b | c |
      | 1 | 2 |
";
    assert_eq!(kinds(src), vec![Kind::RowWidth { expected: 3, found: 2 }]);
}

#[test]
fn a_placeholder_no_examples_column_provides() {
    let src = "\
Feature: T
  Scenario Outline: O
    Given I am <usrename>
    Then <ok>

    Examples:
      | username | ok |
      | ada      | y  |
";
    assert_eq!(kinds(src), vec![Kind::UnknownPlaceholder("usrename".into())]);
}

#[test]
fn a_placeholder_provided_by_any_examples_block_is_fine() {
    let src = "\
Feature: T
  Scenario Outline: O
    Given <a>
    And <b>

    Examples: first
      | a |
      | 1 |

    Examples: second
      | b |
      | 2 |
";
    assert!(validate(src).is_empty(), "{:?}", validate(src));
}

#[test]
fn a_duplicate_examples_column_silently_wins() {
    let src = "\
Feature: T
  Scenario Outline: O
    Given <a>

    Examples:
      | a | a |
      | 1 | 2 |
";
    assert_eq!(kinds(src), vec![Kind::DuplicateColumn("a".into())]);
}

#[test]
fn a_background_has_to_come_first_and_come_once() {
    let after = "Feature: T\n  Scenario: S\n    Given a\n  Background:\n    Given b\n";
    assert_eq!(kinds(after), vec![Kind::BackgroundAfterAScenario]);

    let twice = "Feature: T\n  Background:\n    Given a\n  Background:\n    Given b\n";
    assert_eq!(kinds(twice), vec![Kind::MultipleBackgrounds]);

    let ruled = "\
Feature: T
  Background:
    Given a

  Rule: A rule
    Background:
      Given b

    Example: E
      Given c
";
    assert!(validate(ruled).is_empty(), "{:?}", validate(ruled));
}

#[test]
fn an_unterminated_docstring() {
    for fence in ["\"\"\"", "```"] {
        let src = format!("Feature: T\n  Scenario: S\n    Given a:\n      {fence}\n      text\n");
        assert_eq!(
            kinds(&src),
            vec![Kind::UnterminatedDocstring(fence.to_string())],
            "fence {fence}"
        );
        assert_eq!(at(&src)[0].0, 4, "reported where it OPENED, which is where the fix is");
    }
}

#[test]
fn gherkin_inside_a_docstring_is_text() {
    let src = "\
Feature: T
  Scenario: S
    Given the docs say:
      \"\"\"
      Feature: Not a real one
      Scenario: Nor this
      And a dangling continuation
      | a | b | c |
      \"\"\"
    Then nothing broke
";
    assert!(validate(src).is_empty(), "{:?}", validate(src));
}

#[test]
fn a_language_it_cannot_read_is_declined_rather_than_buried() {
    let src = "# language: de\nFunktionalität: Etwas\n  Szenario: S\n    Gegeben sei a\n";
    assert_eq!(
        kinds(src),
        vec![Kind::UnsupportedLanguage("de".into())],
        "exactly one problem, and NOT a pile of English-keyword complaints"
    );
    assert_eq!(at(src)[0].0, 1);
}

#[test]
fn every_problem_is_reported() {
    let src = "\
Feature: T
  Scenario: Empty
  Scenario Outline: O
    Given <a>
    Examples:
      | b | b |
      | 1 |
";
    let k = kinds(src);
    assert!(k.contains(&Kind::EmptyScenario("Empty".into())), "{k:?}");
    assert!(k.contains(&Kind::DuplicateColumn("b".into())), "{k:?}");
    assert!(k.contains(&Kind::RowWidth { expected: 2, found: 1 }), "{k:?}");
    assert!(k.contains(&Kind::UnknownPlaceholder("a".into())), "{k:?}");
    assert!(k.len() >= 4, "expected all four, got {k:?}");
}

#[test]
fn positions_are_one_based_and_counted_in_characters() {
    // `é` is one character, two bytes: a byte-based column reports 17.
    let src = "\
Feature: T
  Scenario Outline: O
    Given café <naem>

    Examples:
      | name |
      | ada  |
";
    let p = validate(src);
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0].kind, Kind::UnknownPlaceholder("naem".into()));
    assert_eq!((p[0].line, p[0].column), (3, 16), "the `<` is the 16th character");
}

#[test]
fn an_escaped_pipe_does_not_split_a_cell() {
    let src = "\
Feature: T
  Scenario: S
    Given the table:
      | a    | b |
      | x\\|y | 2 |
";
    assert!(validate(src).is_empty(), "an escaped pipe is one cell: {:?}", validate(src));
}

#[test]
fn only_what_is_between_pipes_is_a_cell() {
    let cut = "Feature: T\n  Scenario: S\n    When I press\n      | foo |\n      | bar\n";
    assert_eq!(kinds(cut), vec![Kind::RowWidth { expected: 1, found: 0 }]);

    let backslash = "Feature: T\n  Scenario: S\n    When I press\n      | foo |\n      | bar \\\n";
    assert_eq!(kinds(backslash), vec![Kind::RowWidth { expected: 1, found: 0 }]);

    let extra = "\
Feature: Extra table content
  Scenario: We are a bit extra
    Given a pirate crew
      | Luffy | Zorro | Doflamingo \\
      | Nami  | Brook | BlackBeard
";
    assert!(validate(extra).is_empty(), "{:?}", validate(extra));
}

#[test]
fn a_tag_line_has_to_be_tags() {
    let src = "Feature: T\n\n  @a tag containing whitespace\n  Scenario: S\n    Given a\n";
    assert_eq!(kinds(src), vec![Kind::MalformedTag("tag".into())]);

    let ok = "Feature: T\n\n  @one @two\n  Scenario: S\n    Given a\n";
    assert!(validate(ok).is_empty(), "{:?}", validate(ok));
}

#[test]
fn a_real_dialect_is_declined_and_a_made_up_one_is_an_error() {
    let real = validate("# language: fr\nFonctionnalité: X\n");
    assert_eq!(real.len(), 1);
    assert_eq!(real[0].kind, Kind::UnsupportedLanguage("fr".into()));
    assert_eq!(real[0].severity(), Severity::Declined);

    // No space after `#`, as in the corpus file.
    let fake = validate("#language:no-such\n\nFeature: Minimal\n  Scenario: S\n    Given a\n");
    assert_eq!(fake.len(), 1);
    assert_eq!(fake[0].kind, Kind::InvalidLanguage("no-such".into()));
    assert_eq!(fake[0].severity(), Severity::Error);
}

#[test]
fn one_docstring_per_step() {
    let src =
        "Feature: T\n  Scenario: S\n    Given a step\n\"\"\"\none\n\"\"\"\n\"\"\"\ntwo\n\"\"\"\n";
    assert!(kinds(src).contains(&Kind::RepeatedDocstring), "{:?}", kinds(src));

    let ok = "Feature: T\n  Scenario: S\n    Given a:\n      \"\"\"\none\n      \"\"\"\n    Then b:\n      \"\"\"\ntwo\n      \"\"\"\n";
    assert!(validate(ok).is_empty(), "{:?}", validate(ok));
}

#[test]
fn a_tag_with_nothing_to_tag() {
    let src =
        "Feature: T\n\n  Scenario Outline: minimalistic\n    Given the minimalism\n\n    @tag\n";
    assert!(kinds(src).contains(&Kind::DanglingTag), "{:?}", kinds(src));

    let ok = "Feature: T\n\n  @tag\n  Scenario: S\n    Given a\n";
    assert!(validate(ok).is_empty(), "{:?}", validate(ok));
}

#[test]
fn what_parses_is_a_warning_and_what_does_not_is_an_error() {
    let parses_but_pointless =
        "Feature: T\n  Scenario: Empty\n  Scenario Outline: O\n    Given a\n";
    let p = validate(parses_but_pointless);
    assert!(!p.is_empty());
    assert!(
        p.iter().all(|x| x.severity() == Severity::Warning),
        "these parse: {:?}",
        p.iter().map(|x| (&x.kind, x.severity())).collect::<Vec<_>>()
    );

    let will_not_parse = "Feature: T\n  Scenario: S\n    Given a\n      | x | y |\n      | 1 |\n";
    assert!(validate(will_not_parse).iter().any(|x| x.severity() == Severity::Error));

    let declined = validate("# language: fr\nFonctionnalité: X\n");
    assert_eq!(declined.len(), 1);
    assert_eq!(declined[0].severity(), Severity::Declined);
}

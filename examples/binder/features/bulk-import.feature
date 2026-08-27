# The spreadsheet path, described the way a person would describe it.
#
# These run. `tests/features.rs` parses this file with `gherkin:validate` and drives
# each step against a real `comp-host` serving the composed binder — so a scenario
# that stops being true fails the build, and one nobody implemented fails loudly
# rather than reading as documentation.
@bulk
Feature: A collection that already exists arrives as a spreadsheet

  Bulk entry is the difference between trying this app and using it. Nobody
  retypes four hundred cards.

  Background:
    Given a signed-in collector "sheets@binder.test"

  @xlsx
  Scenario: Four cards from a real .xlsx
    When I upload "cards.xlsx" to "/api/cards/bulk?name=cards.xlsx"
    Then the response is 201
    And the field "added" is 4
    And the field "with_a_purchase" is 4
    And the field "sheet" is "sheet1"

  @xlsx
  Scenario: The spreadsheet's own arithmetic reaches the portfolio
    When I upload "cards.xlsx" to "/api/cards/bulk?name=cards.xlsx"
    And I GET "/api/portfolio"
    Then the response is 200
    # 120000 + 2500x4 + 45000x2 + 18000x3 — the numbers in the sheet, nothing else.
    And the field "cost_basis_minor" is 274000

  @csv
  Scenario: A quoted comma is one field, not two
    When I upload "more.csv" to "/api/cards/bulk?name=more.csv"
    And I GET "/api/cards"
    Then the response is 200
    And the body contains "Mr. Mime, holo"

  Scenario: One bad row writes nothing
    When I upload "bad.csv" to "/api/cards/bulk?name=bad.csv"
    Then the response is 422
    And the field "problems.0.row" is 3
    When I GET "/api/cards"
    Then the field "cards" has 0 entries

  Scenario Outline: A format nobody can read is refused by name
    When I upload "cards.xlsx" to "/api/cards/bulk?name=<name>"
    Then the response is 400

    Examples: things that are not spreadsheets
      | name          |
      | cards.numbers |
      | cards.xls     |
      | cards.pdf     |

  Scenario: Importing the same sheet twice updates rather than duplicates
    When I upload "cards.xlsx" to "/api/cards/bulk?name=cards.xlsx"
    And I upload "cards.xlsx" to "/api/cards/bulk?name=cards.xlsx"
    Then the response is 201
    And the field "added" is 0
    And the field "updated" is 4

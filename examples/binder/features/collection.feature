# Who a collection belongs to, and what happens to somebody who is not them.
@auth
Feature: A collection belongs to somebody

  Checked before anything else, because a route that forgets to introspect reads
  whichever collection it finds and every other scenario would still pass.

  Scenario: No token, no collection
    When I GET "/api/cards" as nobody
    Then the response is 401

  Scenario: A collector sees their own cards and nobody else's
    Given a signed-in collector "alice@binder.test"
    When I POST "/api/cards" with:
      """
      { "name": "Snorlax", "set_code": "base", "number": "11/102" }
      """
    Then the response is 201
    When I GET "/api/cards"
    Then the field "cards" has 1 entries
    Given a signed-in collector "bob@binder.test"
    When I GET "/api/cards"
    Then the field "cards" has 0 entries

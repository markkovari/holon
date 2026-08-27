# The thing `portfolio:value` has described since the day it was written, and the
# app never did:
#
#   "A swap is two events, not one: what left is a disposal at the agreed value and
#    what arrived is an acquisition at that same value, so a trade where you got the
#    better end shows up as a gain."
#
# Four events here, because there are two collections.
@swap
Feature: Two collectors swap a card

  Background:
    Given a signed-in collector "ash@binder.test"
    When I POST "/api/cards" with:
      """
      { "name": "Charizard", "set_code": "base", "number": "4/102", "paid_minor": 120000, "quantity": 1 }
      """
    Then the response is 201

  Scenario: An offer is a square somebody photographs
    When I POST "/api/swaps" with:
      """
      { "give": "base-4-102", "want": "base-2-102", "value_minor": 100000 }
      """
    Then the response is 201
    And the field "value_minor" is 100000
    # The handover is a phone camera, not two people reading an id aloud.
    And the body contains "<svg"

  Scenario: You cannot offer a card you do not have
    When I POST "/api/swaps" with:
      """
      { "give": "base-9-999", "want": "base-2-102", "value_minor": 100000 }
      """
    Then the response is 422

  Scenario: A swap with no agreed value is refused
    When I POST "/api/swaps" with:
      """
      { "give": "base-4-102", "want": "base-2-102", "value_minor": 0 }
      """
    Then the response is 422

  Scenario: The cards change hands and both portfolios agree
    When I POST "/api/swaps" with:
      """
      { "give": "base-4-102", "want": "base-2-102", "value_minor": 100000 }
      """
    Then the response is 201
    And I remember the field "id" as "swap"
    Given a signed-in collector "misty@binder.test"
    When I POST "/api/cards" with:
      """
      { "name": "Blastoise", "set_code": "base", "number": "2/102", "paid_minor": 90000, "quantity": 1 }
      """
    Then the response is 201
    When I POST "/api/swaps/{swap}/accept" with:
      """
      {}
      """
    Then the response is 200
    And the field "events_written" is 4
    # Misty gave Blastoise and holds Charizard.
    When I GET "/api/cards"
    Then the field "cards" has 1 entries
    And the body contains "Charizard"
    # She paid 90000 for it and swapped it at 100000, so the trade made 10000.
    When I GET "/api/portfolio"
    Then the field "realised_minor" is 10000

  Scenario: A swap can only be taken once
    When I POST "/api/swaps" with:
      """
      { "give": "base-4-102", "want": "base-2-102", "value_minor": 100000 }
      """
    And I remember the field "id" as "swap"
    Given a signed-in collector "brock@binder.test"
    When I POST "/api/cards" with:
      """
      { "name": "Blastoise", "set_code": "base", "number": "2/102", "paid_minor": 90000 }
      """
    And I POST "/api/swaps/{swap}/accept" with:
      """
      {}
      """
    Then the response is 200
    When I POST "/api/swaps/{swap}/accept" with:
      """
      {}
      """
    Then the response is 409

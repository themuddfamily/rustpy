@calc
Feature: Calculator
  As a user I want to add numbers.

  Background:
    Given a fresh calculator

  Scenario: Add two numbers
    When I add 2 and 3
    Then the result is 5

  Scenario: Failing addition
    When I add 2 and 2
    Then the result is 5

  Scenario: Exploding step
    When I divide 1 by 0
    Then the result is 0

  @skip
  Scenario: Skipped scenario
    When I add 1 and 1
    Then the result is 2

  Scenario: Undefined step
    When I multiply 2 by 3
    Then the result is 6

  Scenario Outline: Add <a> and <b>
    When I add <a> and <b>
    Then the result is <sum>

    Examples: Small numbers
      | a | b | sum |
      | 1 | 2 | 3   |
      | 2 | 2 | 5   |

  Scenario: Data table and doc string
    Given the following numbers:
      | value |
      | 1     |
      | 2     |
    When I sum them
    Then the result is 3
    And the report says:
      """
      total: 3
      """

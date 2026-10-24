Feature: Rules
  Rule: Positive numbers
    Scenario: Positive
      Given a fresh calculator
      When I add 1 and 2
      Then the result is 3

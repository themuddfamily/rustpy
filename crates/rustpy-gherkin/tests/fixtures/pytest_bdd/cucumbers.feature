Feature: Cucumber basket

  Scenario: Eating cucumbers
    Given I have a basket
    And there are 5 cucumbers
    When I eat 3 cucumbers
    Then I should have 2 cucumbers
    And the basket is "almost empty"

  Scenario Outline: Eating many cucumbers
    Given there are <start> cucumbers
    When I eat <eat> cucumbers
    Then I should have <left> cucumbers

    Examples:
      | start | eat | left |
      | 12    | 5   | 7    |
      | 20    | 5   | 15   |

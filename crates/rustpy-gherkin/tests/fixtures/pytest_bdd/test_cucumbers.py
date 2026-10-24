from pytest_bdd import given, parsers, scenarios, then, when

scenarios("cucumbers.feature")


@given("I have a basket", target_fixture="basket")
def basket():
    return {"count": 0}


@given(parsers.parse("there are {start:d} cucumbers"), target_fixture="basket")
def cucumbers(start):
    return {"count": start}


@when(parsers.parse("I eat {eat:d} cucumbers"))
def eat(basket, eat):
    basket["count"] -= eat


@then(parsers.re(r"I should have (?P<left>\d+) cucumbers"), converters={"left": int})
def should_have(basket, left):
    assert basket["count"] == left


@then(parsers.cfparse('the basket is "{state}"'))
def basket_state(basket, state):
    assert state == "almost empty"

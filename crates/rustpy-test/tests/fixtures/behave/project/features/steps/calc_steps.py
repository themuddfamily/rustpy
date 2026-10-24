from behave import given, when, then, use_step_matcher


@given("a fresh calculator")
def step_fresh(context):
    context.result = 0


@when("I add {a:d} and {b:d}")
def step_add(context, a, b):
    print("adding", a, b)
    context.result = a + b


@when("I divide {a:d} by {b:d}")
def step_divide(context, a, b):
    context.result = a / b


@then("the result is {expected:d}")
def step_result(context, expected):
    assert context.result == expected, "expected %d but got %d" % (expected, context.result)


@given("the following numbers:")
def step_numbers(context):
    context.numbers = [int(row["value"]) for row in context.table]


use_step_matcher("re")


@when("I sum them")
def step_sum(context):
    context.result = sum(context.numbers)


@then(r"the report says:")
def step_report(context):
    assert context.text.strip() == "total: %d" % context.result

from lettuce import step, world


@step('a known value')
def known_value(step):
    world.value = 2


@step('it is retained')
def retained(step):
    assert world.value == 2

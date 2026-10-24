import logging
import sys
from behave import given
try:
    from behave.api.pending_step import StepNotImplementedError
except ImportError:
    StepNotImplementedError = NotImplementedError


@given("a step")
def step_a(context):
    pass


@given("a pending step")
def step_pending(context):
    raise StepNotImplementedError("later")


@given("a noisy failing step")
def step_noisy(context):
    print("to stdout")
    print("to stderr", file=sys.stderr)
    logging.getLogger("x").warning("logged warning")
    assert False, "noisy failed"

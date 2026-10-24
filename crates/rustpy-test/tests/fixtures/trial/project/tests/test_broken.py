import does_not_exist  # noqa: F401
from twisted.trial import unittest


class BrokenTest(unittest.TestCase):
    def test_x(self):
        pass

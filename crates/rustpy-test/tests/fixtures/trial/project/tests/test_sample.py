from twisted.internet import defer
from twisted.trial import unittest


class SampleTest(unittest.TestCase):
    def test_pass(self):
        print("hello from pass")
        self.assertEqual(1 + 1, 2)

    def test_fail(self):
        self.assertEqual(1, 2, "one is not two")

    def test_error(self):
        raise RuntimeError("boom")

    def test_skip(self):
        raise unittest.SkipTest("not today")

    def test_todo(self):
        self.fail("expected to fail")

    test_todo.todo = "known bug"

    def test_unexpected(self):
        pass

    test_unexpected.todo = "should fail but passes"

    def test_deferred_fail(self):
        d = defer.Deferred()
        d.addCallback(lambda _: self.fail("deferred failed"))
        d.callback(None)
        return d


class OtherTest(unittest.TestCase):
    skip = "whole class skipped"

    def test_a(self):
        pass


class SlowTest(unittest.TestCase):
    def test_sleep(self):
        import time

        time.sleep(0.05)

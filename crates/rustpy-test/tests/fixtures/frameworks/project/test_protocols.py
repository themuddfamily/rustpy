import logging
import unittest


class Cases(unittest.TestCase):
    def setUp(self):
        self.value = 2

    def tearDown(self):
        self.value = None

    def test_subtests(self):
        for value in (2, 3):
            with self.subTest(value=value):
                self.assertEqual(2, value)


def test_plain():
    print('ordinary captured stdout')
    assert True

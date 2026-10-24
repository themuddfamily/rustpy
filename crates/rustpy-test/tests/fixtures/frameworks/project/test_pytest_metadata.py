import logging
import pytest


@pytest.mark.slow
@pytest.mark.parametrize('number', [2, 3], ids=['custom pass', 'custom failure'])
def test_with_fixture(supplied_value, number):
    logging.warning('captured record for %s', number)
    assert number == supplied_value

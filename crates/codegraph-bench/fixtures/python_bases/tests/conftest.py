import pytest

from pkg.models.user import Admin


@pytest.fixture
def admin():
    return Admin()

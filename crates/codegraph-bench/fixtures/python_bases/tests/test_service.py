from pkg.service import make_user


def test_admin(admin):
    assert admin.run()


def test_make_user():
    assert make_user()

from pkg import models
from pkg.models import User


def make_user():
    user = User.create()
    other = models.User()
    return user.run(), other.run()

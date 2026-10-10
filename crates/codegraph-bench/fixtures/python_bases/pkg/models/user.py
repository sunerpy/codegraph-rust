from .base import Base, Meta, Mixin


class User(Base, Mixin, metaclass=Meta):
    def run(self):
        super().ping()
        return self.describe()


class Admin(User):
    def run(self):
        return super().run()

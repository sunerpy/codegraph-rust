class Mixin:
    def describe(self):
        return type(self).__name__


class Base:
    @classmethod
    def create(cls):
        return cls()

    def ping(self):
        return 1


class Meta(type):
    pass

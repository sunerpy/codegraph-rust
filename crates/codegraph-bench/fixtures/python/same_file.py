class ReturnClass:
    pass


class AliasClass:
    pass


class RegistryClass:
    pass


class ArgumentClass:
    pass


class ListClass:
    pass


class TupleA:
    pass


class TupleB:
    pass


class HandlerOwner:
    def handler(self):
        return None


def choose_return():
    return ReturnClass


def choose_alias():
    alias = AliasClass


def choose_registry():
    registry = {"registry": RegistryClass}


def choose_argument():
    register(ArgumentClass)


def choose_list():
    values = [ListClass]


def choose_tuple():
    return TupleA, TupleB


def wire(handler):
    register(handler)

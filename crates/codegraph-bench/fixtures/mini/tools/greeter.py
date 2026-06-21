class Greeter:
    def __init__(self, prefix: str) -> None:
        self.prefix = prefix

    def greet(self, name: str) -> str:
        return f"{self.prefix}, {name}"


def make_greeting(name: str) -> str:
    greeter = Greeter("hello")
    return greeter.greet(name)

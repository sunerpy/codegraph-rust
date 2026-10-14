from flask import Flask

app = Flask(__name__)


@app.route("/hello")
def hello():
    return greeting()


def greeting():
    return "hello"


def ping():
    return "pong"


app.add_url_rule("/ping", view_func=ping)

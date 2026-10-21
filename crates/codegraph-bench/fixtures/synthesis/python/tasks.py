from celery import Celery

app = Celery("tasks")


@app.task
def send_email(address):
    return render(address)


def render(address):
    return address


def signup(address):
    send_email.delay(address)

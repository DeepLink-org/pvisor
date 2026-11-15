"""Small real Celery workload; Redis is both broker and persistent result backend."""

import os

from celery import Celery

app = Celery(
    "compose_workload",
    broker=os.environ["CELERY_BROKER_URL"],
    backend=os.environ["CELERY_RESULT_BACKEND"],
)
app.conf.result_expires = 3600


@app.task(name="compose.add")
def add(left, right):
    return left + right

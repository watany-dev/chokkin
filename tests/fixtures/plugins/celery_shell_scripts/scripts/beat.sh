#!/bin/sh
celery -Aworker.beat:app beat

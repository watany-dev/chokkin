#!/bin/sh
celery --app="worker.monitor" flower

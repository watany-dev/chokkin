#!/bin/sh
flask --app "./src/web/factory.py:make_app()" run

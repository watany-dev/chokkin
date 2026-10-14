#!/bin/sh
set -e
exec flask --app myapp.factory:create_app run

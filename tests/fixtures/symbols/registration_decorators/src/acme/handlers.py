import click
from flask_login import LoginManager
from flask_restful import Api
from sqlalchemy import event
from sqlalchemy.engine import Engine

api = Api()
login_manager = LoginManager()


@click.group()
def cli() -> None:
    pass


@cli.group()
def admin() -> None:
    pass


@api.representation("application/json")
def json_representation(data, code, headers=None):
    return data


@event.listens_for(Engine, "after_execute")
def after_execute(conn, *args) -> None:
    pass


@login_manager.unauthorized_handler
def redirect_to_login():
    return None


def dead_helper() -> None:
    pass

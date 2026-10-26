import sqlalchemy
from snowflake.sqlalchemy import URL


def main() -> None:
    sqlalchemy.create_engine(URL())

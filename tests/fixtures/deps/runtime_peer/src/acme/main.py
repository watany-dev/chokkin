from starlette.requests import Request


async def main(request: Request) -> None:
    await request.form()

import functools

from fastapi import APIRouter

router = APIRouter()


@router.get("/items")
def list_items() -> list[str]:
    return []


@router.websocket("/ws")
async def stream(websocket) -> None:
    await websocket.accept()


@functools.cache
def dead_api() -> None:
    """Decorated, but not registered with any framework."""

from fastapi import APIRouter, FastAPI

app = FastAPI()
router = APIRouter(prefix="/items")


@router.get("/{item_id}")
def read_item(item_id: int):
    return lookup(item_id)


def lookup(item_id: int):
    return {"id": item_id}


app.include_router(router)

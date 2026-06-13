def classify(value: int) -> str:
    match value:
        case 0:
            return "zero"
        case _:
            return "other"

def kind(x):
    match x:
        case 0:
            return "zero"
        case 1 | 2 | 3:
            return "small"
        case "s":
            return "str-s"
        case True:
            return "true"
        case None:
            return "none"
        case _:
            return "other"
print(kind(0), kind(2), kind(3), kind("s"), kind(True), kind(None), kind(9), kind([1]))
def both(x):
    match x:
        case 1 | 2:
            return "low"
        case 3 | 4:
            return "high"
print(both(1), both(4), both(9) if True else None)
match 42:
    case 42:
        print("hit")
    case _:
        print("miss")
match "x":
    case "y":
        print("no")
    case _:
        print("wild")

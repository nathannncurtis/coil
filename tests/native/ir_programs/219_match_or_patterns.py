def kind(v):
    match v:
        case 0 | 1:
            return "small"
        case 2 | 3 | 4:
            return "mid"
        case int() as n if n > 100:
            return f"big-{n}"
        case int():
            return "other-int"
        case "yes" | "y" | True:
            return "affirm"
        case [1, *rest] | (1, *rest):
            return f"seq1+{rest}"
        case {"k": a} | {"m": a}:
            return f"map-{a}"
        case _:
            return "unknown"
print(kind(0), kind(1), kind(3), kind(500), kind(50))
print(kind("yes"), kind(True), kind("other"))
print(kind([1, 2, 3]), kind((1,)), kind({"k": 9}), kind({"m": 3}))
print(kind(3.5), kind(None))
match [1, 2]:
    case [x, y] | (x, y):
        print("either", x, y)
match {"a": 1}:
    case {"a": val} | {"b": val}:
        print("mapped", val)
class P:
    __match_args__ = ("x", "y")
    def __init__(self, x, y):
        self.x, self.y = x, y
    def __repr__(self):
        return f"P({self.x},{self.y})"
for v in (P(1, 2), P(0, 5)):
    match v:
        case P(0, y) | P(1, y):
            print("short-x", y)
        case P(x, _):
            print("x", x)
match (1, (2, 3)):
    case (a, (b, c)) | [a, [b, c]]:
        print(a, b, c)
match 5:
    case n if n % 2 == 0:
        print("even", n)
    case n:
        print("odd", n)

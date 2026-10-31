evals = []
class Rec:
    __match_args__ = ("v",)
    def __init__(self, v):
        self.v = v
def guard(x):
    match x:
        case Rec(v) if evals.append("g1") or v > 2:
            return "big"
        case Rec(v) if evals.append("g2") or v > 1:
            return "mid"
        case Rec(v):
            return "small"
        case _:
            return "other"
print(guard(Rec(3)), guard(Rec(2)), guard(Rec(1)), guard("x"))
print(evals)
def multi(x):
    out = []
    match x:
        case [1, 2]:
            out.append("12")
        case [int(a), int(b)] if a < b:
            out.append(f"inc {a} {b}")
        case [int(a), int(b)]:
            out.append(f"other {a} {b}")
        case _:
            out.append("?")
    return out
print(multi([1, 2]), multi([1, 5]), multi([5, 1]), multi(["a"]))

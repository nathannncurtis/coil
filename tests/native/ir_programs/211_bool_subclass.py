class Truthy:
    def __init__(self, v):
        self.v = v
    def __bool__(self):
        return self.v > 2
print(Truthy(1).v, Truthy(5).v)
print(bool(Truthy(1)), bool(Truthy(5)))
print(Truthy(3).v + Truthy(4).v, int(Truthy(9).v))
class Weird:
    def __len__(self):
        return 0
print(bool(Weird()), len(Weird()))
class Half:
    def __len__(self):
        return 7
print(bool(Half()))
class Always:
    def __bool__(self):
        return True
print(bool(Always()), "x" if Always() else "y")
print(bool([]), bool([0]), bool({}), bool({0: 0}), bool(set()), bool({1}))
print(bool(0), bool(1), bool(""), bool("0"), bool(0.0), bool(0.5))
print(bool(()), bool((0,)), bool(None), bool(range(0)), bool(range(1)))
if Truthy(5):
    print("truthy-path")
while Truthy(1):
    print("never")
    break
print(not Truthy(1), not Truthy(9))
print(Truthy(9) and "and-result", Truthy(1) or "or-result")
print(len([1]) and 5, None or "fallback")
class StrLike:
    def __str__(self):
        return "STR"
print(str(StrLike()), f"{StrLike()}")
class ReprLike:
    def __repr__(self):
        return "R()"
print(ReprLike(), [ReprLike()])

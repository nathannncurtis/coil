print(hash(0), hash(1), hash(-1))
print(hash(2 ** 61 - 1) == 2 ** 61 - 1)
print(hash(True), hash(False), hash(3.0) == hash(3))
print(hash(10 ** 30) != 0)
class NoHash:
    __hash__ = None
try:
    hash(NoHash())
except TypeError as e:
    print("TypeError", e)
class EqOnly:
    def __init__(self, v):
        self.v = v
    def __eq__(self, o):
        return isinstance(o, EqOnly) and self.v == o.v
try:
    hash(EqOnly(1))
except TypeError as e:
    print("TypeError", e)
d = {1: "int", True: "bool"}
print(len(d), d[1])
s = {1, True}
print(len(s))
print(hash(0.5), hash(1.5) == hash(1.5))
t = (1, "a")
print(hash(t) == hash((1, "a")) if False else "skip-str-hash")
print(hash(()) == hash(()))

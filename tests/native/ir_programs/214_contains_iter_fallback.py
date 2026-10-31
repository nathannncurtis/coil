class ByContains:
    def __init__(self, items):
        self.items = items
    def __contains__(self, v):
        print("contains", v)
        return v in self.items
bc = ByContains([1, 2, 3])
print(2 in bc, 5 in bc)
class ByIter:
    def __init__(self, items):
        self.items = items
    def __iter__(self):
        print("iter-called")
        return iter(self.items)
bi = ByIter([1, 2])
print(2 in bi, 5 in bi)
print(list(bi))
class Gen:
    def __iter__(self):
        yield 1
        yield 2
print(list(Gen()), 2 in Gen())
class Seq:
    def __getitem__(self, i):
        if i > 3:
            raise IndexError
        print("get", i)
        return i * 10
s = Seq()
print(list(s))
print(30 in s, 5 in s)
print([x for x in s])
class OldStyle:
    def __iter__(self):
        raise TypeError("no iter")
try:
    for x in OldStyle():
        pass
except TypeError as e:
    print("TypeError", e)
print(1 in [1], 1 in (1,), 1 in {1: "a"}, "a" in {"a": 1}, 1 in {1}, 1 in range(5))
print("h" in "hello", b"h" in b"hi", (1, 2) in [(1, 2)])
try:
    print(1 in 5)
except TypeError as e:
    print("TypeError", e)
class Infinite:
    def __contains__(self, v):
        return True
print(12345 in Infinite())
for k in {"a": 1}:
    print("key", k)

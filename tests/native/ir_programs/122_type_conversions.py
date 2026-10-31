print(int(1.9), int(-1.9), float(2), str(3), bool(""), repr("x"))
print(list("abc"), tuple([1, 2]), set([2, 1]) if False else "skip-set")
print(str([1, "a"]), str((1,)), str(None), repr([1, "a", None]))
print(str({"k": 1}), repr({"k": 1}))
print(type(1) is int, type("s") is str, type([]) is list, type({}) is dict)
print(isinstance(1, (str, int)), isinstance("s", (str, int)))
print(isinstance(True, bool), isinstance(True, int), isinstance(1, bool))
class A:
    pass
class B(A):
    pass
b = B()
print(isinstance(b, B), isinstance(b, A), isinstance(b, (int, A)))
print(type(b) is B, type(b) is A)
print(issubclass(B, A), issubclass(A, B), issubclass(B, (int, A)))
print(str(A), type(A).__name__)
print(type(type(1)))

class BadIter:
    def __iter__(self):
        return self
    def __next__(self):
        raise ValueError("bad next")
try:
    for x in BadIter():
        print("never")
except ValueError as e:
    print("iter-error", e)
def bad_gen():
    yield 1
    raise TypeError("bad gen")
try:
    list(bad_gen())
except TypeError as e:
    print("gen-error", e)
class BadLen:
    def __len__(self):
        return "not-int"
try:
    len(BadLen())
except TypeError as e:
    print("len-error", e)
class BadBool:
    def __bool__(self):
        return "not-bool"
try:
    if BadBool():
        pass
except TypeError as e:
    print("bool-error", e)
class BadRepr:
    def __repr__(self):
        raise KeyError("no-repr")
try:
    print(BadRepr())
except KeyError:
    print("repr-error")
class BadStr:
    def __str__(self):
        return 42
try:
    print(BadStr())
except TypeError as e:
    print("str-error", e)
class BadFormat:
    def __format__(self, spec):
        return 42
try:
    print(f"{BadFormat()}")
except TypeError as e:
    print("format-error", e)

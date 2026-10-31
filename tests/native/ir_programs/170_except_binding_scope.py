try:
    raise ValueError("v")
except ValueError as err:
    print("inside", err)
try:
    print(err)
except NameError:
    print("deleted-after-block")
for attempt in range(2):
    try:
        if attempt == 0:
            raise KeyError("k")
        print("second-ok")
    except KeyError as e:
        print("caught", e)
saved = None
try:
    raise ValueError("keep")
except ValueError as e:
    saved = e
print("saved", saved)
def f():
    try:
        raise ValueError("in-f")
    except ValueError as exc:
        return exc
print(f().args[0])
try:
    try:
        raise ValueError("outer-scope")
    except ValueError as e2:
        raise TypeError("inner")
except TypeError as e3:
    print("final", e3)

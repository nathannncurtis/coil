def side(tag, val):
    print("eval", tag)
    return val
print(side("a", 1) and side("b", 2))
print(side("c", 0) and side("d", 2))
print(side("e", []) or side("f", "x"))
print(side("g", "y") or side("h", "x"))
def find(xs, pred):
    for x in xs:
        if pred(x):
            return x
    return None
print(find([1, 2, 3], lambda v: v > 1))
v = 0
result = v or "default"
print(result)
w = "" or [] or {} or "last"
print(w)
def default_arg(a=None):
    return a if a is not None else "auto"
print(default_arg(), default_arg(0), default_arg(False))
print((1, 2) if True else (3, 4))
print([x if x % 2 else -x for x in range(5)])
print(sorted(["b", "A", "c"], key=str.lower if True else None))

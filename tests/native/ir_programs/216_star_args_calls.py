def f(a, b, *c, d=0, **e):
    return (a, b, c, d, sorted(e.items()))
print(f(*range(3)))
print(f(*[1], 2, *[3, 4], d=5, x=6))
def g(*args, **kw):
    return (args, sorted(kw.items()))
print(g(*[], **{}))
print(g(*"ab"))
print(g(*{"k": 1}))
print(g(**{"a": 1}, b=2))
def h(a, b, /, c, *, d):
    return (a, b, c, d)
print(h(1, 2, 3, d=4))
print(h(1, 2, c=3, d=4))
def pack(*a):
    return a
print(pack(), pack(1), pack(1, 2), pack([1], (2,)))
def spread(fn, args, kw):
    return fn(*args, **kw)
print(spread(f, (1, 2, 3), {"d": 9, "z": 1}))
print(max(*[3, 9, 2]))
print(list(zip(*[(1, "a"), (2, "b")])))
print((*[1], *(2, 3)))
def rec(x):
    if isinstance(x, list):
        return rec(x[0]) if x else None
    return x
print(rec([[[[7]]]]))
m = [[1, 2], [3, 4]]
print([*m[0], *m[1]])
def KwOnly(*, k):
    return k * 2
print(KwOnly(k="ab"))
try:
    f()
except TypeError as e:
    print("TypeError", e)

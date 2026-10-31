def f(a, b, c=3, *rest, d=4, **kw):
    return (a, b, c, rest, d, sorted(kw.items()))
print(f(1, 2))
print(f(1, 2, 30, 40, 50, d=9, x=1))
print(f(*[1, 2, 3]))
print(f(**{"a": 1, "b": 2}))
print(f(*[1], 2, *[3], **{"d": 5}))
def g(a, /, b):
    return a + b
print(g(1, 2))
print(g(1, b=2))
try:
    g(a=1, b=2)
except TypeError as e:
    print("TypeError", e)
def h(*, a, b=2):
    return (a, b)
print(h(a=1), h(a=1, b=9))
first, *middle, last = (1, 2, 3, 4)
print(first, middle, last)
(a, b), (c,) = (1, 2), (3,)
print(a, b, c)
for (i, j) in [(1, 2)]:
    print(i + j)
[x, y] = [10, 20]
print(x, y)
(p, q), = [(1, 2)]
print(p, q)
try:
    f()
except TypeError as e:
    print("TypeError", e)

i = 0
def f(a=i):
    return a
i = 9
print(f(), f(1))
calls = []
def trace(x):
    calls.append(x)
    return x
def g(a=trace("eval-once")):
    return a
print(g(), g(2), calls)
def h(a, b=[]):
    if a:
        b.append(a)
    return b
print(h(1), h(0), h(3))
def kw(a, *, b="db", c=None):
    return (a, b, c)
print(kw(1), kw(1, b="B"), kw(1, c=0))
def pos(*, a):
    return a
try:
    pos(1)
except TypeError as e:
    print("TypeError", e)
try:
    pos()
except TypeError as e:
    print("TypeError", e)
def annot(x: int, y: "str" = "d") -> "result":
    return y * x
print(annot(2), annot(2, "z"))
def lam(a=lambda: "lazy"):
    return a()
print(lam(), lam(lambda: "given"))

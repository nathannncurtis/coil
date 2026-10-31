def gen(n):
    for i in range(n):
        yield i * i
g = gen(3)
print(next(g), next(g), next(g))
try:
    next(g)
except StopIteration:
    print("exhausted")
print(list(gen(4)))
def counter(start):
    v = start
    while True:
        got = yield v
        if got is not None:
            v = got
        v += 1
c = counter(10)
print(next(c), next(c))
print(c.send(100))
def echo():
    while True:
        got = yield
        if got == "stop":
            return "bye"
        print("echo", got)
e = echo()
next(e)
e.send(1)
e.send(2)
try:
    e.send("stop")
except StopIteration as si:
    print("returned", si.value)
def one_pass():
    yield 1
    yield 2
    return 99
g2 = one_pass()
print(next(g2))
try:
    next(g2)
except StopIteration as e:
    print("value", e.value)
print(list(gen(0)))
def yield_expr():
    r = yield "a"
    print("got", r)
    r2 = yield "b"
    print("got2", r2)
g3 = yield_expr()
print(next(g3))
print(g3.send("x"))

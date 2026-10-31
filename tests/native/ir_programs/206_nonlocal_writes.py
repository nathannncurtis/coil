def make_counter():
    count = 0
    def bump():
        nonlocal count
        count += 1
        return count
    def get():
        return count
    return bump, get
bump, get = make_counter()
bump(); bump()
print(get())
def outer():
    x = "outer"
    def mid():
        nonlocal x
        x = "mid-set"
        def inner():
            return x
        return inner()
    return mid(), x
print(outer())
def fib():
    memo = {}
    def f(n):
        if n in memo:
            return memo[n]
        r = n if n < 2 else f(n - 1) + f(n - 2)
        memo[n] = r
        return r
    return f
print(fib()(15))
def acc():
    total = 0
    def add(*vals):
        nonlocal total
        for v in vals:
            total += v
        return total
    return add
a = acc()
a(1, 2); a(10)
print(a())
def gen_reset():
    state = 0
    def g():
        nonlocal state
        while True:
            yield state
            state += 1
    def reset():
        nonlocal state
        state = 0
    return g(), reset
g, reset = gen_reset()
print([next(g) for _ in range(3)])
reset()
print(next(g))
class Capture:
    def __init__(self):
        self.log = []
c = Capture()
def outer2():
    def inner(v):
        c.log.append(v)
    for i in range(3):
        inner(i * i)
outer2()
print(c.log)

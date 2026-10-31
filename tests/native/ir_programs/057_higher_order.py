def apply_n(f, n, x):
    for _ in range(n):
        x = f(x)
    return x
print(apply_n(lambda v: v * 2, 4, 1))
def make_adders():
    return [lambda x, s=s: x + s for s in range(3)]
print([f(10) for f in make_adders()])
def fold(f, xs, acc):
    for x in xs:
        acc = f(acc, x)
    return acc
print(fold(lambda a, b: a + b, range(5), 0))
print(fold(lambda a, b: a * b, range(1, 5), 1))
def outer_fn():
    return [x for x in map(str, range(3))]
print(outer_fn())
calls = []
def traced(f):
    def w(v):
        calls.append(v)
        return f(v)
    return w
@traced
def double(v):
    return v * 2
print(double(2), double(3), calls)

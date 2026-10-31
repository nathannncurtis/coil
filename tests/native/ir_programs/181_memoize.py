def memoize(f):
    cache = {}
    calls = [0]
    def wrapper(n):
        calls[0] += 1
        if n not in cache:
            cache[n] = f(n)
        return cache[n]
    wrapper.calls = lambda: calls[0]
    wrapper.cache = cache
    return wrapper
@memoize
def slow_double(n):
    return n * 2
print(slow_double(4), slow_double(4), slow_double.calls())
print(slow_double.cache)
def fib_memo(n, cache={}):
    if n in cache:
        return cache[n]
    r = n if n < 2 else fib_memo(n - 1) + fib_memo(n - 2)
    cache[n] = r
    return r
print(fib_memo(30), len(fib_memo.__defaults__[0]) if False else "cache-used")
class MemoAttr:
    def __init__(self):
        self._sq = {}
    def square(self, n):
        if n not in self._sq:
            self._sq[n] = n * n
        return self._sq[n]
ma = MemoAttr()
print(ma.square(9), ma.square(9), ma._sq)
def count_calls(f):
    n = [0]
    def w(*a):
        n[0] += 1
        return f(*a)
    return w, n
wrapped, counter = count_calls(lambda x: x + 1)
wrapped(1)
wrapped(2)
print(counter[0])

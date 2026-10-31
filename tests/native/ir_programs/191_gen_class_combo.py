class Fibs:
    def __init__(self, n):
        self.n = n
    def __iter__(self):
        a, b = 0, 1
        for _ in range(self.n):
            yield a
            a, b = b, a + b
print(list(Fibs(7)))
class Grid:
    def __init__(self, rows):
        self.rows = rows
    def cells(self):
        for r, row in enumerate(self.rows):
            for c, v in enumerate(row):
                yield (r, c, v)
print(list(Grid([[1, 2], [3, 4]]).cells()))
def make_counters(k):
    return [(lambda i=i: i * k) for i in range(3)]
print([f() for f in make_counters(10)])
class GenMethod:
    def __init__(self, xs):
        self.xs = xs
    def pairs(self):
        it = iter(self.xs)
        for a in it:
            b = next(it, None)
            yield (a, b)
print(list(GenMethod([1, 2, 3, 4, 5]).pairs()))
def infinite():
    i = 0
    while True:
        yield i
        i += 1
inf = infinite()
print([next(inf) for _ in range(5)])
def gen_with_state():
    history = []
    for i in range(3):
        history.append(i)
        yield history
print([list(h) for h in gen_with_state()])
class ClsGen:
    count = 0
    def __init__(self):
        ClsGen.count += 1
    @classmethod
    def all_counts(cls):
        for i in range(cls.count):
            yield i
[ClsGen() for _ in range(4)]
print(list(ClsGen.all_counts()))

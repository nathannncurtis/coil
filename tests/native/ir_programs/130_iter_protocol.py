xs = [1, 2]
it = iter(xs)
print(next(it), next(it))
print(iter(xs) is xs if False else "skip")
d = {"a": 1, "b": 2}
print(list(iter(d)))
print(list(d.items()), list(d.keys()), list(d.values()))
it2 = iter(d.items())
print(next(it2))
class Countdown:
    def __init__(self, n):
        self.n = n
    def __iter__(self):
        return self
    def __next__(self):
        if self.n <= 0:
            raise StopIteration("done-value")
        self.n -= 1
        return self.n + 1
print(list(Countdown(3)), max(Countdown(5)), sum(Countdown(4)))
for x in Countdown(2):
    print("cd", x)
def gen_to_iter():
    yield "g1"
    yield "g2"
print(list(iter(gen_to_iter())))
gg = gen_to_iter()
print(iter(gg) is gg)
print(list(zip(Countdown(3), "abc")))
print(dict(zip("ab", [1, 2])))
class Squares:
    def __getitem__(self, i):
        if i > 3:
            raise IndexError
        return i * i
print(list(Squares()), 9 in Squares())
n = 0
for v in Squares():
    n += v
print(n)
print(tuple(Countdown(2)), sorted(Countdown(4), reverse=True))
print(list(enumerate(Countdown(3))))

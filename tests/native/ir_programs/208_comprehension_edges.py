print([x for x in range(0)])
print([x for x in range(3) if x if x > 0])
print([x if x % 2 else -x for x in range(5)])
print([(x, y) for x in "ab" for y in (1,) if x != "b"])
d = {"k": 1}
print([k for k in d], [v for v in d.values()], [(k, v) for k, v in d.items()])
print({v: k for k, v in d.items()})
s = {1, 2}
print(sorted({x + y for x in s for y in s}))
print([len(w) for w in [] ])
print([1 for _ in "xyz"])
print([[y + 1 for y in row] for row in [[0], [1, 2]]])
def side():
    print("side")
    return [1, 2]
print([x for x in side() if print("check", x) is None])
xs = [1, 2, 3]
print([xs[i] for i in range(len(xs))])
print([i for i, v in enumerate("abc")])
print([a + b for a, b in zip([1, 2], (10, 20))])
print({k: [v] for k, v in ((1, 1), (2, 2))})
print([x for x in (i * i for i in range(4))])
print([[c for c in w] for w in ["hi", "yo"]])
try:
    print([1 / x for x in (2, 0, 4)])
except ZeroDivisionError as e:
    print("ZeroDivisionError", e)
print([(x, [x]) for x in range(2)])

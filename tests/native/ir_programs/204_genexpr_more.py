xs = [1, 2, 3, 4, 5, 6]
print(list(x for x in xs))
print([x * x for x in xs if x % 2 == 0])
print([y for x in xs if x > 2 for y in (x, -x)])
print([(a, b) for a in range(3) for b in range(a)])
print({x: x * x for x in range(4)})
print({x % 3 for x in xs})
print(sum(x for x in xs))
print(max(len(w) for w in ["a", "bb", "ccc"]))
g = (x + 1 for x in xs)
print(next(g), next(g))
print(list(g))
def counter(n):
    i = 0
    while i < n:
        yield i
        i += 1
print([a + b for a in counter(3) for b in counter(2)])
print(list(filter(lambda v: v > 2, xs)))
print(list(zip(xs, (s + "!" for s in "abc"))))
matrix = [[1, 2], [3, 4]]
print([row[i] for i in range(2) for row in matrix])
print([[r[i] for r in matrix] for i in range(2)])
print(list(map(lambda *a: sum(a), [1, 2], [10, 20])))
nests = [[x for x in range(r)] for r in range(4)]
print(nests)
print([x for x in (y * 2 for y in xs) if x % 4 == 0])

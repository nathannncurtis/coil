def make():
    n = 3
    return (x * n for x in range(4))
print(list(make()))
g = (i + j for i in range(3) for j in range(2))
print(list(g))
print(list((i, j) for i in "ab" for j in (1,)))
print(max(x ** 2 for x in range(-3, 4)))
print(any(x > 2 for x in range(5)), all(x > 2 for x in range(5)))
total = 0
for v in (i for i in range(5)):
    total += v
print(total)
matrix = [[1, 2], [3, 4]]
print([x for row in matrix for x in row])
print([[r[i] for r in matrix] for i in range(2)])
first = [x for x in range(3)]
print(first, [x for x in first])
def gen_in_gen():
    return list(a + b for a in (i for i in range(2)) for b in (10, 20))
print(gen_in_gen())
leak = "before"
[leak := x for x in "ab"]
print(leak)

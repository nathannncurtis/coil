pairs = [(1, "a"), (2, "b")]
for n, c in pairs:
    print(n, c)
for (a, b), in [((1, 2),), ((3, 4),)]:
    print(a + b)
for [p, q] in [[1, 2], [3, 4]]:
    print(p * q)
d = {"x": 1}
for k in d:
    print(k, d[k])
for i, c in enumerate("ab"):
    print(i, c)
for i, c in enumerate("abc", 10):
    print(i, c)
for a, b in zip([1, 2], "xy"):
    print(a, b)
for row in [[1], [2, 2], [3, 3, 3]]:
    print(len(row))

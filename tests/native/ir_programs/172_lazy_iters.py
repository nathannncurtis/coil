m = map(lambda x: x * 2, [1, 2, 3])
print(next(m))
print(list(m))
f = filter(lambda x: x % 2, range(6))
print(next(f), list(f))
z = zip("ab", [1, 2, 3])
print(next(z), list(z))
e = enumerate("abc")
print(next(e), list(e))
r = reversed([1, 2, 3])
print(next(r), list(r))
def consume(it):
    out = []
    for v in it:
        out.append(v)
        if len(out) >= 2:
            break
    return out, list(it)
print(consume(iter(range(10))))
squares = map(lambda v: v * v, range(5))
print(sum(squares), sum(squares) if False else "exhausted")
chained = map(str, map(lambda v: v + 1, [1, 2]))
print(list(chained))
print(list(zip(*[(1, 2), (3, 4)])))
print(list(map(lambda a, b: a + b, [1, 2], [10, 20])))

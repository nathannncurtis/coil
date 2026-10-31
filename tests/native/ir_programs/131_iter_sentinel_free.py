def walk_nested(x):
    if isinstance(x, list):
        for y in x:
            yield from walk_nested(y)
    else:
        yield x
print(list(walk_nested([1, [2, [3, 4], 5], []])))
def flatmap(f, xs):
    for x in xs:
        yield from f(x)
print(list(flatmap(lambda v: (v, v * 10), [1, 2])))
def take(it, n):
    out = []
    for i, v in enumerate(it):
        if i >= n:
            break
        out.append(v)
    return out
print(take(walk_nested([1, [2, [3]]]), 2))
def chunked(xs, n):
    it = iter(xs)
    while True:
        chunk = list(take(it, n))
        if not chunk:
            return
        yield chunk
print(list(chunked(range(7), 3)))
print(list(x for x in walk_nested([[1], 2]) if x > 0))

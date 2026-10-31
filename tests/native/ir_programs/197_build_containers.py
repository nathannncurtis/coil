print([])
print([1], [1, 2], [1, 2, 3])
print(())
print((1,), (1, 2))
print({})
print({1: 2}, {"a": [1]})
print(set(), {1}, {1, 2})
print([1, "two", 3.0, None, True])
print([[1], (2,), {3}, {"k": 4}])
print((1, (2, (3,))))
print({"outer": {"in": [1, 2]}})
n = 3
print([0] * n, [i for i in range(n)], {i: i for i in range(n)})
print(list(range(n)), tuple(range(n)))
xs = [1, 2]
print([*xs, *xs], (*xs, *xs))
print({*xs, 9})
print({"a": 1, **{"b": 2}, "c": 3})
def f(*a, **k):
    return (a, sorted(k.items()))
print(f(*[1, 2], **{"x": 1}))
print([[], []], [[[]]])
print(len([[], [1]]))
mixed = [1]
mixed += [2]
mixed.append([3])
print(mixed, len(mixed))
print([xs, *range(2)])

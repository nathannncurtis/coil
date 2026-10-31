def f(a, b, c):
    return (a, b, c)
args = [1, 2]
kw = {"c": 3}
print(f(*args, **kw))
print(f(1, *[9, 8]))
print(f(*[1, 2, 3]))
print(f(**{"a": 1, "b": 2, "c": 3}))
print(f(1, c=3, b=2))
def g(*xs, **kw):
    return (xs, sorted(kw.items()))
print(g(*[1, 2], *[3], **{"a": 1}))
lst = [3, 1]
print(sorted(*[lst]))
print([*lst, *range(2)])
print({"a": 1, **{"b": 2}, "c": 3})
print((*range(3),))
print([*[1], *(2, 3)])
t = (f, (1, 2, 3))
print(t[0](*t[1]))

t = (1, 2, 3)
print(t, len(t), t[0], t[-1])
print(t + (4,), t * 2)
print(1 in t, 9 not in t)
print(t.index(2), t.count(3))
print((), (5,), (1,))
u = 1, 2
print(u, u == (1, 2))
a, b = u
print(a, b)
nested = ((1, 2), (3, 4))
print(nested[1])
print(tuple([1, 2]), tuple("ab"), tuple(range(3)))
print(sorted(t, reverse=True))
try:
    t[0] = 9
except TypeError as e:
    print("TypeError", e)

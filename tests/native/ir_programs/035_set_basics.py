s = {1, 2, 3}
print(len(s), 1 in s, 9 not in s)
s.add(4)
s.add(1)
print(len(s), s)
print(sorted(s))
s.discard(9)
s.remove(1)
print(sorted(s))
a = {1, 2, 3}
b = {2, 3, 4}
print(sorted(a | b), sorted(a & b), sorted(a - b), sorted(a ^ b))
print(a.union(b), sorted(a.intersection(b)), sorted(a.difference(b)))
print(a.issubset({1, 2, 3, 4}), b.issuperset({2, 3}))
print({i * i for i in range(6)})
print(set([3, 1, 2]), set("aabbc") if False else "skip")
f = frozenset([1, 2, 3])
print(len(f), 2 in f, sorted(f | {4}))
try:
    f.add(9)
except AttributeError as e:
    print("AttributeError", e)
try:
    {[]}
except TypeError as e:
    print("TypeError", e)

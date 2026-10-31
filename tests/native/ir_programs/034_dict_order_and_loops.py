d = {}
for i in [3, 1, 2]:
    d[f"k{i}"] = i * 10
print(list(d))
for k in d:
    print(k, d[k])
total = 0
for v in d.values():
    total += v
print(total)
ks = [k for k in d if d[k] > 10]
print(ks)
print({v: k for k, v in d.items()})
d2 = {k: v for k, v in [("z", 1), ("y", 2)]}
print(d2)
n = {}
for i in range(5):
    n[i % 2] = n.get(i % 2, 0) + i
print(n)
print(len({}), bool({}), bool({0: 0}))

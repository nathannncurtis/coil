d = {1: "a", "b": 2, (3, 4): "tuple-key", True: "bool-key"}
print(d[1], d["b"], d[(3, 4)], d[True])
print(len(d))
d[1] = "changed"
print(len(d), d[1])
print((3, 4) in d, 7 not in d)
nested = {"outer": {"in": {"deep": 42}}}
print(nested["outer"]["in"]["deep"])
nested["outer"]["in"]["deeper"] = 43
print(nested["outer"]["in"])
counts = {}
for w in "a b a c b a".split():
    counts[w] = counts.get(w, 0) + 1
print(counts, sorted(counts.items()))
inverted = {v: k for k, v in {"a": 1, "b": 2}.items()}
print(inverted)
merged = {**{"a": 1, "b": 2}, **{"b": 3, "c": 4}}
print(merged)
xs = []
for k, v in merged.items():
    xs.append(f"{k}={v}")
print(xs)
keys = list({"z": 1, "a": 2})
print(keys)
try:
    {[]: 1}
except TypeError as e:
    print("TypeError", e)
class Hashable:
    def __init__(self, v):
        self.v = v
    def __hash__(self):
        return self.v
    def __eq__(self, o):
        return isinstance(o, Hashable) and self.v == o.v
h = {Hashable(1): "one"}
print(h[Hashable(1)])

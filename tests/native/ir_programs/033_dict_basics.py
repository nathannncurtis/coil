d = {"a": 1, "b": 2, "c": 3}
print(d, len(d))
print(d["a"], d.get("b"), d.get("z"), d.get("z", 99))
d["d"] = 4
d["a"] = 10
print(d)
print(list(d.keys()), list(d.values()), list(d.items()))
print("a" in d, "z" in d, "a" not in d)
del d["b"]
print(d)
print(d.pop("c"), d.pop("zz", "fallback"))
print(d.setdefault("e", 5), d.setdefault("a", 0))
d.update({"f": 6}, g=7)
print(d)
print(d.popitem())
e = {}
e["x"] = 1
e["y"] = 2
print(e)
print(dict(a=1, b=2), dict([("k", 1), ("j", 2)]), dict({"q": 9}))
print({**d, "extra": 8} if True else None)
try:
    d["missing"]
except KeyError as e:
    print("KeyError", e)

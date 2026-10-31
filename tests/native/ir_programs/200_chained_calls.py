class Builder:
    def __init__(self):
        self.parts = []
    def add(self, v):
        self.parts.append(str(v))
        return self
    def wrap(self, pre, post):
        self.parts.insert(0, pre)
        self.parts.append(post)
        return self
    def build(self, sep=""):
        return sep.join(self.parts)
print(Builder().add(1).add(2).wrap("[", "]").build())
print(Builder().add("a").add("b").build("-"))
print("hello".upper().lower().title())
print("a,b,c".split(",").index("b"))
print([3, 1, 2].sort() if False else sorted([3, 1, 2]).__len__())
print("  x  ".strip().ljust(4, "."))
d = {"a": 1, "b": 2}
print(sorted(d.items(), key=lambda kv: kv[1]))
print(list(d.keys()).index("b"))
print(str(42).__len__())
print("abc".replace("b", "x").upper()[0])
print(float("3.5").is_integer())
xs = [5, 2, 8]
print(xs.copy() if hasattr(xs, "copy") else list(xs))
print(sorted(xs, reverse=True)[0])
print("-".join(sorted("cba")))

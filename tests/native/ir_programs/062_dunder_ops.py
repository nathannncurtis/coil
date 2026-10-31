class Vec:
    def __init__(self, *cs):
        self.cs = list(cs)
    def __add__(self, o):
        return Vec(*[a + b for a, b in zip(self.cs, o.cs)])
    def __mul__(self, k):
        return Vec(*[c * k for c in self.cs])
    def __rmul__(self, k):
        return self * k
    def __sub__(self, o):
        return Vec(*[a - b for a, b in zip(self.cs, o.cs)])
    def __neg__(self):
        return Vec(*[-c for c in self.cs])
    def __eq__(self, o):
        return isinstance(o, Vec) and self.cs == o.cs
    def __lt__(self, o):
        return sum(self.cs) < sum(o.cs)
    def __len__(self):
        return len(self.cs)
    def __getitem__(self, i):
        return self.cs[i]
    def __contains__(self, v):
        return v in self.cs
    def __repr__(self):
        return "Vec" + repr(tuple(self.cs))
a = Vec(1, 2)
b = Vec(3, 4)
print(a + b, a - b, a * 3, 3 * a, -a)
print(a == Vec(1, 2), a == b, a != b)
print(a < b, b < a, len(a), a[1], 2 in a, 9 in a)
print(sum(a))
class StrLike:
    def __str__(self):
        return "str!"
    def __repr__(self):
        return "repr!"
s = StrLike()
print(s, repr(s), str(s), f"{s}")
print(f"{s!r}")
class Callable:
    def __call__(self, x):
        return x + 1
print(Callable()(41), callable(Callable()), callable(3))

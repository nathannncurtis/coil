class Bag:
    def __init__(self, xs):
        self.xs = xs
    def __contains__(self, v):
        return self.xs.count(v) > 1
    def __iter__(self):
        return iter(self.xs)
    def __len__(self):
        return len(self.xs)
    def __getitem__(self, i):
        return self.xs[i]
b = Bag([1, 1, 2, 3, 3])
print(1 in b, 2 in b, 3 not in b)
print(list(b), len(b), b[2])
print(9 in Bag([9]), [x for x in b if x > 1])
class Reversable:
    def __init__(self, xs):
        self.xs = xs
    def __len__(self):
        return len(self.xs)
    def __getitem__(self, i):
        return self.xs[i]
print(list(reversed(Reversable([1, 2, 3]))))
class OnlyGetItem:
    def __getitem__(self, i):
        if i >= 2:
            raise IndexError
        return i
print(1 in OnlyGetItem(), list(OnlyGetItem()))
for v in OnlyGetItem():
    print("ogi", v)
class DunderContains:
    def __contains__(self, v):
        return v == "yes"
print("yes" in DunderContains(), "no" in DunderContains())
xs = [1, 2, 3]
print(2 in xs, 2 not in xs, "2" in [1, 2] if False else "skip")
print("h" in "hi", "z" not in "hi", 1 in range(3), 9 in range(3))
d = {"k": None}
print("k" in d, None in d, "k" in d.keys())

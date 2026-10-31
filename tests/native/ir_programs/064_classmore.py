class InstanceCounted:
    n = 0
    names = []
    def __init__(self, name):
        InstanceCounted.n += 1
        self.name = name
        InstanceCounted.names.append(name)
a = InstanceCounted("a")
b = InstanceCounted("b")
print(InstanceCounted.n, InstanceCounted.names, a.n)
class Hooked:
    def __getattr__(self, name):
        return f"<{name}>"
    def __setattr__(self, name, value):
        object.__setattr__(self, name, value) if False else None
        super().__setattr__(name, value)
hk = Hooked()
hk.real = 1
print(hk.real, hk.missing)
class Boolish:
    def __bool__(self):
        return False
    def __len__(self):
        return 3
print(bool(Boolish()) if True else "x")
class LenOnly:
    def __len__(self):
        return 0
print(bool(LenOnly()))
class Iter:
    def __init__(self, xs):
        self.xs = xs
        self.i = 0
    def __iter__(self):
        return self
    def __next__(self):
        if self.i >= len(self.xs):
            raise StopIteration
        v = self.xs[i if False else self.i]
        self.i += 1
        return v
print(list(Iter([1, 2, 3])), [x * 2 for x in Iter([4, 5])])
class Seq:
    def __getitem__(self, i):
        if i >= 3:
            raise IndexError
        return i * i
print(list(Seq()), 4 in Seq())

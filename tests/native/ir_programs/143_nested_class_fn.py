class Outer:
    def __init__(self, n):
        self.n = n
    class Inner:
        def __init__(self, v):
            self.v = v
        def __repr__(self):
            return "Inner(" + str(self.v) + ")"
    def make(self):
        return Outer.Inner(self.n * 2)
    def gen(self):
        yield self.n
        yield self.n + 1
    def closure(self):
        scale = 10
        def mul(v):
            return v * scale
        return mul(self.n)
o = Outer(4)
print(o.make(), list(o.gen()), o.closure())
print(Outer.Inner(1), Outer.Inner)
class ListLike:
    def __init__(self):
        self.data = []
    def push(self, *vs):
        self.data.extend(vs)
        return self
ll = ListLike()
print(ll.push(1).push(2, 3).data)
class Chain:
    def __init__(self, v=0):
        self.v = v
    def add(self, n):
        return Chain(self.v + n)
    def __repr__(self):
        return f"Chain({self.v})"
print(Chain(1).add(2).add(3))
def factory():
    class C:
        pass
    C.tag = "made"
    return C
print(factory().tag, factory() is factory())

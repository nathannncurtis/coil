class A:
    def who(self):
        return "A"
    def hello(self):
        return "hello from " + self.who()
class B(A):
    def who(self):
        return "B"
class C(A):
    def who(self):
        return "C"
class D(B, C):
    pass
d = D()
print(d.who(), d.hello())
print([k.__name__ for k in D.__mro__])
class Base:
    def __init__(self, v):
        self.v = v
class Mid(Base):
    def __init__(self, v, extra):
        super().__init__(v)
        self.extra = extra
class Leaf(Mid):
    def __init__(self):
        super().__init__(1, 2)
print(Leaf().v, Leaf().extra)
class Two:
    def __init__(self):
        super(Two, self).__init__()
print(Two().x if False else "ok")
print(isinstance(d, A), isinstance(d, B), isinstance(d, C))
print(issubclass(D, A), issubclass(D, B))
class Overriding(A):
    def hello(self):
        return "overridden"
print(Overriding().hello(), A().hello())

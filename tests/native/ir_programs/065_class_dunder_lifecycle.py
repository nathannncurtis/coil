log = []
class Tracked:
    def __init__(self, n):
        log.append("init " + n)
        self.n = n
    def __del__(self):
        log.append("del " + self.n)
t = Tracked("t")
t.__del__()
print(log)
del t
class NewStyle:
    def __new__(cls, v):
        print("new", v)
        return super().__new__(cls)
    def __init__(self, v):
        print("init", v)
        self.v = v
n = NewStyle(7)
print(n.v)
class Meta:
    def __init_subclass__(cls, **kw):
        print("subclass", cls.__name__, sorted(kw.items()))
class Child(Meta, tag="t"):
    pass
print(Child.__name__)
class Named:
    def __set_name__(self, owner, name):
        print("setname", owner.__name__, name)
class HasNamed:
    f = Named()
print("done")
class ReprStr:
    def __repr__(self):
        return "R"
    def __str__(self):
        return "S"
v = ReprStr()
print(v, repr(v), str(v), [v], f"{v}", f"{v!r}")
print("x" + str(v))

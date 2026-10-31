class A:
    def __init__(self, x):
        if x < 0:
            raise ValueError("negative")
        self.x = x
try:
    A(-1)
except ValueError as e:
    print("ValueError", e)
print(A(5).x)
class B:
    def __init__(self):
        raise RuntimeError("boom")
    def hello(self):
        return "hi"
try:
    B()
except RuntimeError as e:
    print("RuntimeError", e)
class C:
    def __new__(cls, v):
        print("new", cls.__name__, v)
        return super().__new__(cls)
    def __init__(self, v):
        self.v = v * 2
c = C(4)
print(c.v, isinstance(c, C))
class D:
    def __new__(cls):
        return 42
print(D(), type(D()) is int)
class E:
    def __init__(self):
        self.steps = ["init"]
    def __repr__(self):
        return "E" + repr(self.steps)
print(E())
class F:
    def __init__(self, a, b=2, *rest):
        self.args = (a, b, rest)
print(F(1).args, F(1, 9, 10, 11).args)
class G(A):
    def __init__(self):
        super().__init__(3)
        self.extra = True
g = G()
print(g.x, g.extra)
class H:
    def __init__(self):
        try:
            raise KeyError("k")
        except KeyError as e:
            self.err = str(e)
print(H().err)

class Registry:
    items = []
    @classmethod
    def register(cls, item):
        cls.items.append(item)
        return len(cls.items)
class A(Registry):
    pass
class B(Registry):
    pass
print(A.register("a1"), B.register("b1"), Registry.register("r1"))
print(Registry.items, A.items is Registry.items)
class Counter3:
    n = 0
    def __init__(self):
        Counter3.n += 1
        self.id = Counter3.n
a = Counter3()
b = Counter3()
print(a.id, b.id, Counter3.n, a.n)
a.n = 100
print(a.n, Counter3.n)
class Config:
    settings = {"debug": False}
    @classmethod
    def set(cls, k, v):
        cls.settings[k] = v
Config.set("debug", True)
print(Config.settings, Config().settings)
class Shadow2:
    n = "class"
    def read(self):
        return self.n
s = Shadow2()
print(s.read())
s.n = "inst"
print(s.read(), Shadow2.n)
del s.n
print(s.read())

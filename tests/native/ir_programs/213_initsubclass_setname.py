class Tracker:
    registry = []
class Base:
    def __init_subclass__(cls, **kw):
        Tracker.registry.append((cls.__name__, sorted(kw.items())))
        super().__init_subclass__()
class A(Base, tag="a"):
    pass
class B(Base, tag="b", extra=1):
    pass
class C(A):
    pass
for entry in Tracker.registry:
    print(entry)
class Named:
    def __set_name__(self, owner, name):
        self.name = name
    def __get__(self, obj, objtype=None):
        return f"<{self.name}>" if obj is None else f"{self.name}:{obj.__class__.__name__}"
class Point:
    x = Named()
    y = Named()
    def __init__(self):
        self.z = "plain"
p = Point()
print(Point.x, p.x, p.y, p.z)
class Desc:
    def __set_name__(self, owner, name):
        self.public = name
        self.slot = "_" + name
    def __get__(self, obj, objtype=None):
        return None if obj is None else getattr(obj, self.slot)
    def __set__(self, obj, val):
        setattr(obj, self.slot, val * 10)
class Config:
    size = Desc()
cfg = Config()
cfg.size = 3
print(cfg.size, Config.size, vars(cfg))
class Meta:
    def __init_subclass__(cls, register=True, **kw):
        if register:
            cls.registered = True
        super().__init_subclass__()
class M1(Meta, register=False):
    pass
class M2(Meta):
    pass
print(hasattr(M1, "registered"), M2.registered)
print([c.__name__ for c in A.__mro__])

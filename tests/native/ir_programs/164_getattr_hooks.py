class Proxy:
    def __init__(self, target):
        self._target = target
    def __getattr__(self, name):
        return getattr(self._target, name)
p = Proxy([3, 1, 2])
print(p.append is p.append if False else "skip")
print(len(p._target), sorted(p._target) if False else "skip2")
class Logging:
    def __getattribute__(self, name):
        print("get", name)
        return object.__getattribute__(self, name)
l = Logging()
l.a = 1
print(l.a)
class Strict:
    def __init__(self):
        self.ok = "yes"
s = Strict()
try:
    s.bad
except AttributeError as e:
    print("AttributeError", e)
class Guard:
    def __init__(self):
        object.__setattr__(self, "real", 1)
    def __getattr__(self, name):
        if name == "magic":
            return "sparkle"
        raise AttributeError("no " + name)
g = Guard()
print(g.real, g.magic)
try:
    g.other
except AttributeError as e:
    print(e)
class SetHooks:
    def __setattr__(self, name, value):
        super().__setattr__("store_" + name, value)
sh = SetHooks()
sh.x = 9
print(sh.store_x)
try:
    sh.x
except AttributeError:
    print("no plain attr")

class V:
    def __init__(self):
        self.a = 1
        self.b = 2
    def m(self):
        self.c = 3
v = V()
print(sorted(vars(v)))
v.m()
print(sorted(vars(v)))
print(vars(v)["a"])
g = globals()
g["injected"] = 42
print(injected)
def f():
    return globals() is g
print(f())
print(sorted(k for k in vars(V) if not k.startswith("__")))

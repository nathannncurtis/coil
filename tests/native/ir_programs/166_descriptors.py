class Typed:
    def __init__(self, ty, name):
        self.ty = ty
        self.name = name
    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        return obj.__dict__["_" + self.name]
    def __set__(self, obj, value):
        if not isinstance(value, self.ty):
            raise TypeError(self.name + " must be " + self.ty.__name__)
        obj.__dict__["_" + self.name] = value
class Rec:
    x = Typed(int, "x")
    y = Typed(str, "y")
r = Rec()
r.x = 5
r.y = "hi"
print(r.x, r.y)
try:
    r.x = "bad"
except TypeError as e:
    print("TypeError", e)
print(Rec.x.name, type(Rec.x).__name__ if False else "desc")
class NonData:
    def __get__(self, obj, objtype=None):
        return "nondata"
class Holder:
    nd = NonData()
h = Holder()
print(h.nd)
h.nd = "shadowed"
print(h.nd)
del h.nd
print(h.nd)
class ClsAccess:
    def __get__(self, obj, objtype=None):
        return ("instance" if obj else "class", objtype.__name__)
class CA:
    d = ClsAccess()
print(CA.d)
print(CA().d)

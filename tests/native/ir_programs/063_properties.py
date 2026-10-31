class Temp:
    def __init__(self):
        self._c = 0
    @property
    def celsius(self):
        return self._c
    @celsius.setter
    def celsius(self, v):
        self._c = v
    @celsius.deleter
    def celsius(self):
        self._c = 0
    @property
    def fahrenheit(self):
        return self._c * 9 / 5 + 32
t = Temp()
t.celsius = 100
print(t.celsius, t.fahrenheit)
del t.celsius
print(t.celsius)
class RO:
    @property
    def v(self):
        return 42
r = RO()
print(r.v)
try:
    r.v = 1
except AttributeError as e:
    print("AttributeError", e)
class Desc:
    def __get__(self, obj, objtype=None):
        return "got-" + (obj.name if obj else "class")
    def __set__(self, obj, value):
        obj.name = value.upper()
class Holder:
    desc = Desc()
    def __init__(self, name):
        self.name = name
h = Holder("bob")
print(h.desc)
h.desc = "alice"
print(h.name)
print(Holder.desc)

class C:
    klass = "K"
    def m(self):
        return "method"
c = C()
print(getattr(c, "klass"), getattr(c, "nope", "dflt"))
print(getattr(c, "m")())
setattr(c, "new", 7)
print(c.new, getattr(c, "new"))
print(hasattr(c, "m"), hasattr(c, "zz"))
delattr(c, "new")
print(hasattr(c, "new"))
try:
    getattr(c, "missing")
except AttributeError as e:
    print("AttributeError", e)
try:
    delattr(c, "also-missing")
except AttributeError as e:
    print("AttributeError", e)
d = {"k": "v"}
print(getattr(d, "get")("k"))
lst = [1]
print(getattr(lst, "append") is lst.append if False else "skip")
name = "klass"
print(getattr(C, name))
print(getattr(1, "real"), getattr(-1.5, "is_integer")())
print(callable(print), callable(3), callable(C))
print(getattr("s", "upper")())
print(C.klass, getattr(C, "klass"))

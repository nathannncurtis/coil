d = {"a": 1, "b": 2}
del d["a"]
print(d)
xs = [1, 2, 3]
del xs[0]
print(xs)
t = (1, 2, 3)
del t
try:
    t
except NameError:
    print("deleted-name")
class C:
    pass
c = C()
c.v = 1
del c.v
try:
    c.v
except AttributeError:
    print("deleted-attr")
y = 1
z = 2
del y, z
try:
    y + z
except NameError as e:
    print("NameError")
xs2 = [0, 1, 2, 3]
del xs2[1:3]
print(xs2)
def f():
    local = 1
    del local
    return "del-ok"
print(f())

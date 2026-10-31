def add(a, b):
    return a + b
print(add(2, 3), add(b=4, a=1))
def defaults(a, b=2, c=3):
    return (a, b, c)
print(defaults(1), defaults(1, 9), defaults(1, c=9))
def kwonly(a, *, b, c=3):
    return (a, b, c)
print(kwonly(1, b=2), kwonly(1, b=2, c=9))
def posonly(a, b, /, c):
    return (a, b, c)
print(posonly(1, 2, 3), posonly(1, 2, c=3))
def varargs(*args, **kwargs):
    return (args, sorted(kwargs.items()))
print(varargs(1, 2, x=3, y=4))
print(varargs())
def mixed(a, b=1, /, c=2, *rest, d=3, **kw):
    return (a, b, c, rest, d, sorted(kw.items()))
print(mixed(9))
print(mixed(9, 8, 7, 6, 5, d=0, z=1))
try:
    defaults()
except TypeError as e:
    print("TypeError", e)
try:
    add(1)
except TypeError as e:
    print("TypeError", e)
try:
    add(1, 2, 3)
except TypeError as e:
    print("TypeError", e)

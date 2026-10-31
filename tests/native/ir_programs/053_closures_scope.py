x = "module"
def reader():
    return x
print(reader())
def writer():
    global x
    x = "changed"
writer()
print(x)
def outer():
    v = "outer-v"
    def rd():
        return v
    def wr():
        nonlocal v
        v = "inner-v"
    rd()
    wr()
    return (rd(), v)
print(outer())
def shadow():
    y = 1
    def inner(y):
        return y
    return (y, inner(99))
print(shadow())
def uses_default_calls(n=[]):
    n.append(1)
    return n
print(uses_default_calls(), uses_default_calls())
try:
    def bad():
        return nope
    bad()
except NameError as e:
    print("NameError", e)

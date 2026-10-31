counter = 0
def bump():
    global counter
    counter += 1
bump()
bump()
print(counter)
name = "initial"
def rebind():
    global name
    name = "rebound"
rebind()
print(name)
def reader_g():
    return counter
print(reader_g())
def outer_g():
    def inner():
        global made_by_inner
        made_by_inner = 5
    inner()
outer_g()
print(made_by_inner)
d = {}
def store(k, v):
    global d
    d[k] = v
store("a", 1)
print(d)
def deep():
    def deeper():
        global deep_val
        deep_val = 99
    deeper()
    return deep_val
print(deep())

def g():
    yield 1
    raise KeyError("in-generator")
for v in g():
    print(v)

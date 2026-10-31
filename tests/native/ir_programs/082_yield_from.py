def inner(n):
    for i in range(n):
        yield i
    return "inner-done"
def outer(n):
    r = yield from inner(n)
    print("got", r)
    yield "after"
print(list(outer(3)))
def chain_a():
    yield 1
    yield 2
def chain_b():
    yield from chain_a()
    yield 3
def chain_c():
    yield from chain_b()
    yield 4
print(list(chain_c()))
def delegating():
    got = yield from inner(2)
    return got
g = delegating()
print(next(g), next(g))
try:
    next(g)
except StopIteration as e:
    print(e.value)
def send_through():
    yield from middle()
def middle():
    g = sub()
    got = yield from g
    print("middle got", got)
def sub():
    while True:
        v = yield "s"
        if v == "end":
            return "sub-ret"
st = send_through()
print(next(st))
print(st.send("x"))
try:
    st.send("end")
except StopIteration:
    print("pipeline-done")
print(sum(i for i in range(10)))
print(list(x * 2 for x in [1, 2, 3] if x != 2))

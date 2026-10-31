def accumulator():
    total = 0
    while True:
        term = yield total
        if term is None:
            break
        total += term
    return total
acc = accumulator()
next(acc)
print(acc.send(5))
print(acc.send(7))
try:
    acc.send(None)
except StopIteration as e:
    print("total", e.value)
def averager():
    s = c = 0
    avg = None
    while True:
        v = yield avg
        s += v
        c += 1
        avg = s / c
av = averager()
next(av)
print(av.send(2))
print(av.send(4))
def delegating_averager():
    a = averager()
    next(a)
    result = yield from a
    return result
def passthrough():
    got = yield "ready"
    return "pt-" + got
pt = passthrough()
print(next(pt))
try:
    pt.send("x")
except StopIteration as e:
    print(e.value)
def closeable_range(n):
    try:
        for i in range(n):
            yield i
    except GeneratorExit:
        print("clean exit at", i)
        raise
cr = closeable_range(10)
print(next(cr), next(cr))
cr.close()
print("closed")
g = closeable_range(3)
print(list(g))
g.close()

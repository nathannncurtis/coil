def g():
    try:
        yield 1
        yield 2
    except ValueError as e:
        yield "ve:" + str(e)
    except TypeError:
        yield "te"
    finally:
        yield "fin"
gg = g()
print(next(gg))
print(gg.throw(ValueError("v")))
print(next(gg))
g2 = g()
print(next(g2), next(g2))
try:
    next(g2)
except StopIteration:
    print("si")
def g3():
    try:
        yield "a"
    finally:
        yield "cleanup"
x = g3()
print(next(x))
print(x.close() if False else "skip")
try:
    x.close()
except RuntimeError as e:
    print("RuntimeError", e)
def g4():
    count = 0
    while True:
        try:
            yield count
        finally:
            count += 1
y = g4()
print(next(y), next(y), next(y))

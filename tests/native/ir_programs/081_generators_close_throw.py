def closable():
    try:
        yield 1
        yield 2
    finally:
        print("closing")
g = closable()
print(next(g))
g.close()
print("closed")
def resists():
    try:
        yield 1
    except ValueError:
        yield 99
g2 = resists()
print(next(g2))
print(g2.throw(ValueError("t")))
try:
    next(g2)
except StopIteration:
    print("done")
def catcher():
    try:
        yield 1
    except GeneratorExit:
        print("exit-seen")
        raise
g3 = catcher()
next(g3)
g3.close()
def with_finally():
    try:
        yield "a"
    finally:
        print("fin")
g4 = with_finally()
print(next(g4))
g4.close()
def throws_out():
    yield 1
    raise ValueError("from-gen")
g5 = throws_out()
next(g5)
try:
    next(g5)
except ValueError as e:
    print("caught", e)
def close_after_exhaustion():
    yield 1
g6 = close_after_exhaustion()
list(g6)
g6.close()
print("ok")

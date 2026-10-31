def g():
    try:
        yield 1
        yield 2
    finally:
        yield 99
        print("cleanup")
gen = g()
print(next(gen))
try:
    print("throw->", gen.throw(ValueError("mid")))
except ValueError as e:
    print("throw raised", e)
try:
    print("next->", next(gen))
except StopIteration:
    print("stopped")
except ValueError as e:
    print("next raised", e)
gen2 = g()
print(next(gen2))
try:
    gen2.close()
except RuntimeError as e:
    print("RuntimeError", e)
gen3 = g()
print(next(gen3), next(gen3))
try:
    gen3.close()
except RuntimeError as e:
    print("RuntimeError", e)
def h():
    try:
        while True:
            try:
                yield "tick"
            except ValueError:
                print("caught-throw")
                yield "recovered"
    except GeneratorExit:
        print("exit")
g3 = h()
print(next(g3))
print(g3.throw(ValueError()))
print(next(g3))
g3.close()
def deep():
    try:
        try:
            yield 1
        finally:
            print("f1")
    finally:
        print("f2")
g4 = deep()
print(next(g4))
g4.close()
def yielding_cleanup():
    try:
        yield "a"
    finally:
        for v in ("x", "y"):
            yield v
g5 = yielding_cleanup()
print(next(g5))
print(next(g5))
try:
    next(g5)
except StopIteration:
    print("done")

def f():
    try:
        return "from-try"
    finally:
        print("finally-runs")
print(f())
def g():
    try:
        return "try"
    finally:
        return "finally"
print(g())
def h():
    try:
        raise ValueError("v")
    finally:
        return "swallow"
print(h())
def k():
    out = []
    try:
        out.append("t")
        return out
    finally:
        out.append("f")
print(k())
def m():
    for i in range(3):
        try:
            if i == 1:
                return i
        finally:
            print("fin", i)
    return -1
print(m())
def n():
    try:
        try:
            raise KeyError("inner")
        finally:
            print("inner-finally")
    except KeyError as e:
        return ("caught", str(e))
print(n())
def p():
    try:
        raise ValueError
    except ValueError:
        return "except"
    finally:
        print("outer-finally")
print(p())
def q():
    while True:
        try:
            break
        finally:
            print("loop-finally")
    return "done"
print(q())
def r():
    try:
        return 1
    finally:
        try:
            return 2
        finally:
            print("nested-fin")
print(r())
def s():
    try:
        raise TypeError("t")
    finally:
        raise ValueError("f")
try:
    s()
except ValueError as e:
    print("ValueError", e)

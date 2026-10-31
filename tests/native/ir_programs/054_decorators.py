def wrap(f):
    def inner(*a, **k):
        print("before")
        r = f(*a, **k)
        print("after")
        return r
    return inner
@wrap
def greet(n):
    return "hi " + n
print(greet("bob"))
def tag(t):
    def deco(f):
        def inner(x):
            return f"<{t}>{f(x)}</{t}>"
        return inner
    return deco
@tag("b")
@tag("i")
def txt(v):
    return "x" + v
print(txt("!"))
def loud(f):
    def inner(*a):
        print("call", a[-1])
        return f(*a)
    return inner
class Thing:
    @loud
    def m(self, v):
        return v * 2
t = Thing()
print(t.m(21))
@staticmethod
def sm(x):
    return x + 1
print(sm(1))
def count_calls(f):
    def inner(*a):
        inner.n += 1
        return f(*a)
    inner.n = 0
    return inner
@count_calls
def ping():
    return "pong"
ping()
ping()
print(ping.n)

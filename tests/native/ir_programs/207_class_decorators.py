def add_repr(cls):
    def __repr__(self):
        return f"{cls.__name__}(vars={sorted(vars(self))})"
    cls.__repr__ = __repr__
    return cls
def shout_methods(cls):
    for name in list(vars(cls)):
        if callable(getattr(cls, name)) and not name.startswith("__"):
            setattr(cls, name, (lambda f: lambda self, *a: str(f(self, *a)).upper())(getattr(cls, name)))
    return cls
@add_repr
@shout_methods
class Thing:
    def greet(self, who):
        return "hi " + who
t = Thing()
t.x = 1
print(t)
print(t.greet("bob"))
def count_calls(func):
    n = 0
    def wrapper(*a, **k):
        nonlocal n
        n += 1
        return (n, func(*a, **k))
    return wrapper
@count_calls
def double(v):
    return v * 2
print(double(3), double(4), double(5))
def repeat(k):
    def deco(fn):
        def inner(*a):
            return [fn(*a) for _ in range(k)]
        return inner
    return deco
@repeat(3)
def word():
    return "go"
print(word())
class Deco:
    def __init__(self, fn):
        self.fn = fn
        self.calls = 0
    def __call__(self, *a):
        self.calls += 1
        return self.fn(*a)
@Deco
def triple(v):
    return v * 3
print(triple(2), triple(3), triple.calls)

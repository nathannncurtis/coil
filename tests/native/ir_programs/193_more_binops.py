class Num:
    def __init__(self, v):
        self.v = v
    def __add__(self, o):
        return Num(self.v + (o.v if isinstance(o, Num) else o))
    __radd__ = __add__
    def __sub__(self, o):
        return Num(self.v - (o.v if isinstance(o, Num) else o))
    def __rsub__(self, o):
        return Num((o.v if isinstance(o, Num) else o) - self.v)
    def __mul__(self, o):
        return Num(self.v * (o.v if isinstance(o, Num) else o))
    def __truediv__(self, o):
        return Num(self.v / (o.v if isinstance(o, Num) else o))
    def __floordiv__(self, o):
        return Num(self.v // (o.v if isinstance(o, Num) else o))
    def __mod__(self, o):
        return Num(self.v % (o.v if isinstance(o, Num) else o))
    def __pow__(self, o):
        return Num(self.v ** (o.v if isinstance(o, Num) else o))
    def __repr__(self):
        return f"Num({self.v})"
a = Num(10)
b = Num(3)
print(a + b, a - b, b - a, a * b)
print(a / b, a // b, a % b, a ** b)
print(a + 1, 1 + a, 5 - a, a - 5)
print((a + b) * 2, Num(2) ** Num(3))
class AbsLike:
    def __abs__(self):
        return -42
    def __round__(self, n=None):
        return 7 if n is None else (7, n)
    def __invert__(self):
        return "inverted"
x = AbsLike()
print(abs(x), round(x), round(x, 2), ~x)
print(round(Num(0) if False else 2.5), abs(-2.5), round(-2.567, 1))
print(divmod(Num(7) if False else 7, 2))

class Money:
    def __init__(self, cents):
        self._cents = cents
    @property
    def euros(self):
        return self._cents / 100
    @property
    def cents(self):
        return self._cents
    @cents.setter
    def cents(self, v):
        if v < 0:
            raise ValueError("negative")
        self._cents = v
    @property
    def pretty(self):
        return f"{self._cents // 100}.{abs(self._cents) % 100:02d}"
m = Money(1234)
print(m.euros, m.cents, m.pretty)
m.cents = 205
print(m.cents, m.pretty)
try:
    m.cents = -1
except ValueError as e:
    print("ValueError", e)
class Circ:
    def __init__(self, r):
        self.r = r
    @property
    def area(self):
        return 3.0 * self.r * self.r
c = Circ(2)
print(c.area)
c.r = 3
print(c.area)
class Lazy:
    @property
    def expensive(self):
        print("computing")
        return 99
lz = Lazy()
print("before")
print(lz.expensive)
class InheritProp(Circ):
    @property
    def area(self):
        return super().area * 2
print(InheritProp(2).area)
class RO2:
    def __init__(self):
        self._v = 1
    @property
    def v(self):
        return self._v
ro = RO2()
try:
    ro.v = 2
except AttributeError:
    print("read-only")
print(ro.v)

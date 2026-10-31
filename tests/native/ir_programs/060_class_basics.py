class Point:
    dims = 2
    def __init__(self, x, y):
        self.x = x
        self.y = y
    def mag2(self):
        return self.x ** 2 + self.y ** 2
    def __repr__(self):
        return f"Point({self.x}, {self.y})"
p = Point(3, 4)
print(p, p.x, p.mag2(), Point.dims, p.dims)
p.x = 30
print(p.mag2())
p.z = 9
print(p.z, sorted(vars(p)))
del p.z
print(hasattr(p, "z"), hasattr(p, "x"))
print(isinstance(p, Point), isinstance(p, object), isinstance(3, Point))
print(issubclass(Point, object), issubclass(bool, int))
print(Point.__name__, type(p).__name__)
print(type(p) is Point, type(Point))
class Empty:
    pass
e = Empty()
print(repr(e).split(" object")[0], isinstance(e, Empty))
class WithDoc:
    "docstring here"
    pass
print(WithDoc.__doc__ if hasattr(WithDoc, "__doc__") else "no-doc")

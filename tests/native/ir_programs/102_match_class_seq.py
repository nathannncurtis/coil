class Pt:
    __match_args__ = ("x", "y")
    def __init__(self, x, y):
        self.x = x
        self.y = y
class Line:
    __match_args__ = ("a", "b")
    def __init__(self, a, b):
        self.a = a
        self.b = b
def where(s):
    match s:
        case Pt(x=0, y=0):
            return "origin"
        case Pt(0, y):
            return f"on-y {y}"
        case Pt(x, 0):
            return f"on-x {x}"
        case Pt(x=x, y=y):
            return f"point {x} {y}"
        case Line(a=Pt(x=0), b=b):
            return "line-from-y"
        case _:
            return "unknown"
print(where(Pt(0, 0)), where(Pt(0, 5)), where(Pt(3, 0)), where(Pt(1, 2)))
print(where(Line(Pt(0, 9), Pt(1, 1))))
print(where("str"))
def seq(pt):
    match pt:
        case Pt(x, y) if x == y:
            return "diag"
        case Pt(x, y) if x > y:
            return "wide"
        case Pt(x, y):
            return "tall"
print(seq(Pt(1, 1)), seq(Pt(2, 1)), seq(Pt(1, 2)))
class Node:
    __match_args__ = ("val", "left", "right")
    def __init__(self, val, left=None, right=None):
        self.val = val
        self.left = left
        self.right = right
def total(n):
    match n:
        case None:
            return 0
        case Node(val, left, right):
            return val + total(left) + total(right)
print(total(Node(1, Node(2), Node(3, Node(4)))))

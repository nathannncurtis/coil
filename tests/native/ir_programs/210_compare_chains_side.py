log = []
def v(name, val):
    log.append(name)
    return val
print(1 < v("a", 2) < v("b", 3))
print(log)
log.clear()
print(v("c", 5) > v("d", 4) > v("e", 3))
print(log)
log.clear()
print(1 < v("f", 0) < v("never", 9))
print(log)
print(1 == 1 == 1, 1 == 1 == 2)
print(3 > 2 > 1 > 0, "a" < "b" < "c")
print(1 < 2 > 0, 1 > 2 < 3)
class C:
    def __init__(self, k):
        self.k = k
    def __eq__(self, o):
        return isinstance(o, C) and self.k == o.k
    def __lt__(self, o):
        return self.k < o.k
print(C(1) < C(2), C(2) == C(2), C(3) > C(2))
print(C(1) < C(2) < C(3), C(1) < C(5) < C(3))
xs = [1, 2, 3]
print(0 < len(xs) <= 3 != 4)
print(True == 1, False == 0, True + True)
print(None == None, None is None)
print([1, 2] == [1, 2], (1,) != (1,), {1} == {1})

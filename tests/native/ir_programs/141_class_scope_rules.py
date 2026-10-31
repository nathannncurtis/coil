x = "module-x"
class C:
    x = "class-x"
    y = x + "-read"
    z = [x for i in range(2)]
    def m(self):
        return x
    def n(self):
        return self.x
print(C.x, C.y, C.z, C().m(), C().n())
def make_cls(tag):
    class Inner:
        label = tag
        def get(self):
            return label
    return Inner
I = make_cls("t1")
print(I.label)
try:
    print(I().get())
except NameError as e:
    print("NameError", e)
class HasComp:
    vals = [1, 2, 3]
    doubled = [v * 2 for v in vals]
    total = sum(vals)
print(HasComp.doubled, HasComp.total)
class UsesOuter:
    ref = x
print(UsesOuter.ref)
def outer_fn():
    v = 5
    class Caps:
        w = v + 1
        def get(self):
            return v
    return Caps()
o = outer_fn()
print(o.w, o.get())
class Late:
    a = 1
    b = a + 1
    del a
print(hasattr(Late, "a"), Late.b)

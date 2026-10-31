class O:
    def name(self):
        return "O"
    def chain(self):
        return "O"
class A(O):
    def name(self):
        return "A"
    def chain(self):
        return "A>" + super().chain()
class B(O):
    def name(self):
        return "B"
    def chain(self):
        return "B>" + super().chain()
class C(A, B):
    def chain(self):
        return "C>" + super().chain()
c = C()
print(c.name())
print(c.chain())
print([k.__name__ for k in C.__mro__])
class D(B, A):
    pass
print([k.__name__ for k in D.__mro__])
print(D().chain())
class E(O):
    def use_b_super(self):
        return super().name()
print(E().use_b_super())
class F(A):
    def __init__(self, v):
        super().__init__()
        self.v = v
    def chain(self):
        return f"F({self.v})>" + super().chain()
print(F(7).chain())
class G(C):
    def chain(self):
        return "G>" + super().chain()
print(G().chain())
print(C.__bases__ == (A, B), len(C.__mro__) == 4)

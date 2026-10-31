class A:
    def __init__(self):
        self.trace = ["A"]
    def who(self):
        return "A"
class B(A):
    def __init__(self):
        super().__init__()
        self.trace.append("B")
    def who(self):
        return "B->" + super().who()
    def via_closure(self):
        helper = lambda: super().who()
        return "closure:" + helper()
class C(B):
    def __init__(self):
        super().__init__()
        self.trace.append("C")
    def who(self):
        return "C->" + super().who()
    def in_gen(self):
        yield super().who()
c = C()
print(c.trace)
print(c.who())
print(c.in_gen().__next__())
class D(B):
    def method_with_super(self):
        def inner():
            return super(D, self).who()
        return inner()
print(D().method_with_super())
class E(A):
    def __init__(self):
        super(E, self).__init__()
        self.trace = ["E-via-2arg"]
print(E().trace)
class F(A):
    def calls_in_comp(self):
        return [super().who() for _ in range(2)]
print(F().calls_in_comp())
class G(A):
    def __init__(self):
        super().__init__()
        class Nested(A):
            def hi(self2):
                return "nested-hi"
        self.n = Nested().hi()
g = G()
print(g.trace, g.n)

class Base:
    shared = "base-shared"
    def method(self):
        return "base-method"
class Mid(Base):
    mid_only = "mid"
    def method(self):
        return "mid+" + super().method()
class Leaf(Mid):
    def method(self):
        return "leaf+" + super().method()
l = Leaf()
print(l.method())
print(l.shared, l.mid_only)
print(Leaf.shared, Mid.shared, Base.shared)
l.shared = "instance"
print(l.shared, Leaf.shared)
Leaf.shared = "leaf-class"
print(l.shared, Leaf.shared, Mid.shared)
del l.shared
print(l.shared)
class AddsAttrs(Base):
    pass
a = AddsAttrs()
a.dynamic = 1
print(a.dynamic, a.shared)
print(sorted(set(vars(a))))
class OverridingBase:
    found = "base"
    def locate(self):
        return self.found
class OverridingChild(OverridingBase):
    found = "child"
print(OverridingChild().locate())
class NoOverride:
    x = 1
class Shadow(NoOverride):
    x = 2
print(NoOverride().x, Shadow().x, NoOverride.x, Shadow.x)

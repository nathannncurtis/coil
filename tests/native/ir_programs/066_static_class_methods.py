class Methods:
    count = 0
    def instance_m(self):
        return "inst " + type(self).__name__
    @staticmethod
    def static_m(x):
        return "static " + str(x)
    @classmethod
    def class_m(cls):
        return "class " + cls.__name__
    @classmethod
    def bump(cls):
        cls.count += 1
        return cls.count
m = Methods()
print(m.instance_m(), m.static_m(1), m.class_m())
print(Methods.static_m(2), Methods.class_m(), Methods.instance_m(m))
print(Methods.bump(), Methods.bump(), m.count)
class Sub(Methods):
    pass
print(Sub.class_m(), Sub.bump(), Methods.count)
print(m.instance_m == m.instance_m if False else "skip")
outer = Methods.static_m
print(outer(9))
cm = Methods.class_m
print(cm())

class Box:
    def __init__(self, v):
        self.v = v
    def __len__(self):
        return self.v
    def __format__(self, spec):
        return "B" + spec
print(len(Box(3)), len([1]), len("abc"), len({1: 2}), len((1,)), len({1}))
if len(Box(4)) == 4:
    print("len-in-condition")
while len(Box(0)):
    print("never")
print(f"{Box(1)}")
print(format(Box(2), "spec"))
print(f"{len(Box(5))}")
xs = [1, 2]
print(f"len={len(xs)} sum={xs[0] + xs[1]}")
print(f"{3.5:06.2f}")
print(format(1) if False else "skip")

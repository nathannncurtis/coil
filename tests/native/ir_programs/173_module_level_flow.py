print("start")
x = 1
if x == 1:
    y = "one"
else:
    y = "other"
print(y)
while x < 4:
    x += 1
print(x)
for i in range(2):
    print("loop", i)
try:
    z = 1 / 0
except ZeroDivisionError:
    z = "handled"
print(z)
class _FileLike:
    def __enter__(self):
        return "filelike"
    def __exit__(self, *exc):
        return False
def open_like():
    return _FileLike()
def module_fn():
    return "mf"
class ModuleClass:
    v = module_fn()
print(ModuleClass.v)
with open_like() as f:
    print("with", f)
result = module_fn()
print(result)
values = [module_fn(), ModuleClass.v, "end"]
print(values)
print("end")

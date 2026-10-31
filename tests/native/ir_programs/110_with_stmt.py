class CM:
    def __init__(self, name, log, swallow=False):
        self.name = name
        self.log = log
        self.swallow = swallow
    def __enter__(self):
        self.log.append("enter " + self.name)
        return self.name
    def __exit__(self, et, ev, tb):
        self.log.append("exit " + self.name + " " + (et.__name__ if et else "None"))
        return self.swallow
log = []
with CM("a", log) as n:
    log.append("body " + n)
print(log)
log = []
with CM("swallow", log, swallow=True):
    raise ValueError("hidden")
print(log, "survived")
log = []
try:
    with CM("prop", log):
        raise KeyError("k")
except KeyError:
    print("propagated")
print(log)
log = []
with CM("outer", log), CM("inner", log):
    log.append("body-nested")
print(log)
log = []
with CM("x", log) as a, CM("y", log) as b:
    log.append(a + b)
print(log)
class raises_on_enter:
    def __enter__(self):
        raise RuntimeError("enter-fail")
    def __exit__(self, *exc):
        return False
try:
    with raises_on_enter():
        print("never")
except RuntimeError as e:
    print(e)
log = []
for i in range(2):
    with CM(f"loop{i}", log):
        if i == 1:
            break
print(log)
def with_return():
    with CM("ret", log):
        return "returned"
print(with_return(), log)

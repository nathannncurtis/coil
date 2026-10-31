class Step:
    def __init__(self, v):
        self.v = v
    def __repr__(self):
        return f"Step({self.v})"
    def __await__(self):
        yield self
        return self.v
async def add3(a, b, c):
    x = await Step(a)
    y = await Step(b)
    z = await Step(c)
    return x + y + z
c = add3(1, 2, 3)
print(c.send(None), c.send(None), c.send(None))
try:
    c.send(None)
except StopIteration as e:
    print(e.value)
async def nested():
    inner = await add3(10, 20, 30)
    return inner * 2
c2 = nested()
while True:
    try:
        c2.send(None)
    except StopIteration as e:
        print("nested", e.value)
        break
async def awaits_gen():
    for v in (i for i in range(3)):
        await Step(v)
    return "looped"
c3 = awaits_gen()
steps = 0
while True:
    try:
        c3.send(None)
        steps += 1
    except StopIteration as e:
        print(e.value, steps)
        break
class Failing:
    def __await__(self):
        yield 1
        raise ValueError("await-fail")
async def bad():
    await Failing()
    return "no"
c4 = bad()
c4.send(None)
try:
    c4.send(None)
except ValueError as e:
    print("caught", e)
async def try_await():
    try:
        await Failing()
    except ValueError as e:
        return "recovered:" + str(e)
c5 = try_await()
c5.send(None)
try:
    c5.send(None)
except StopIteration as e:
    print(e.value)

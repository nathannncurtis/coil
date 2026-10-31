def drive(agen, n):
    results = []
    it = agen.__aiter__()
    for _ in range(n):
        async def step():
            return await it.__anext__()
        results.append(run(step))
    return results
def run(coro):
    steps = []
    def step():
        try:
            coro.send(None)
        except StopIteration as e:
            return ("ret", e.value)
        return None
    while True:
        try:
            coro.send(None)
        except StopIteration as e:
            return e.value
async def numbers(k):
    for i in range(k):
        await nil()
        yield i
async def nil():
    return None
print(run(numbers(4).__anext__()))
class Loop:
    def __init__(self):
        self.pending = []
    def run(self, coro):
        try:
            coro.send(None)
        except StopIteration as e:
            return e.value
        raise RuntimeError("not finished in one step")
loop = Loop()
async def square(xs):
    out = []
    for x in xs:
        await nil()
        out.append(x * x)
    return out
print(loop.run(square([1, 2, 3])))
async def one():
    return 1
async def add(a, b):
    return await a() + await b()
print(loop.run(add(one, one)))
async def double_nested():
    async def inner():
        return await one()
    return await inner() * 2
print(loop.run(double_nested()))
async def fail():
    await nil()
    raise KeyError("in-async")
try:
    loop.run(fail())
except KeyError as e:
    print("KeyError", e)

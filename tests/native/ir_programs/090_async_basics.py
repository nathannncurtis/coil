class Awaitable:
    def __await__(self):
        yield 1
        yield 2
        return "aw-ret"
async def coro():
    r = await Awaitable()
    return r
async def simple():
    return 42
c = simple()
try:
    c.send(None)
except StopIteration as e:
    print("direct", e.value)
c2 = coro()
print(c2.send(None))
print(c2.send(None))
try:
    c2.send(None)
except StopIteration as e:
    print("result", e.value)
async def adds():
    a = await Awaitable()
    b = await Awaitable()
    return a + b
c3 = adds()
print(c3.send(None), c3.send(None))
try:
    print(c3.send(None))
except StopIteration as e:
    print(e.value)
async def with_for():
    total = 0
    async for v in AsyncIter([1, 2, 3]):
        total += v
    return total
class AsyncIter:
    def __init__(self, xs):
        self.it = iter(xs)
    def __aiter__(self):
        return self
    async def __anext__(self):
        try:
            return next(self.it)
        except StopIteration:
            raise StopAsyncIteration
c4 = with_for()
try:
    while True:
        c4.send(None)
except StopIteration as e:
    print(e.value)

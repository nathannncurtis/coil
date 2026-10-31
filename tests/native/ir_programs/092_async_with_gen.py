class ACM:
    def __init__(self, name, log):
        self.name = name
        self.log = log
    async def __aenter__(self):
        self.log.append("enter " + self.name)
        return self
    async def __aexit__(self, et, ev, tb):
        self.log.append("exit " + self.name)
        return False
log = []
async def use():
    async with ACM("a", log) as cm:
        log.append("body " + cm.name)
    async with ACM("b", log):
        log.append("body-b")
        raise ValueError("in-b")
async def runner():
    try:
        await use()
    except ValueError as e:
        log.append("caught " + str(e))
    return "ran"
def drive(coro):
    while True:
        try:
            coro.send(None)
        except StopIteration as e:
            return e.value
print(drive(runner()))
print(log)
async def agen():
    for i in range(3):
        yield i
ag = agen()
print(drive(ag.asend(None)))
try:
    drive(ag.asend(None))
except StopAsyncIteration:
    print("stopped")
async def aclosing():
    try:
        yield 1
        yield 2
    finally:
        print("agen-finally")
ag2 = aclosing()
print(drive(ag2.asend(None)))
drive(ag2.aclose())
print("after-close")
async def collected():
    return [x async for x in AsyncIter([1, 2, 3])]
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
print(drive(collected()))
async def simple_acomp():
    return [v async for v in AsyncIter("xy")]
print(drive(simple_acomp()))

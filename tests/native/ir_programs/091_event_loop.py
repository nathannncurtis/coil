class Nap:
    def __init__(self, ticks):
        self.ticks = ticks
    def __await__(self):
        for _ in range(self.ticks):
            yield self
        return "napped"
class Loop:
    def __init__(self):
        self.ready = []
    def spawn(self, coro):
        self.ready.append(coro)
    def run(self):
        ticks = 0
        while self.ready:
            ticks += 1
            coro = self.ready.pop(0)
            try:
                event = coro.send(None)
            except StopIteration as e:
                print("done:", e.value)
                continue
            self.ready.append(coro)
        return ticks
async def worker(name, ticks):
    r = await Nap(ticks)
    return name + "/" + r
loop = Loop()
loop.spawn(worker("w0", 1))
loop.spawn(worker("w1", 3))
print("ticks", loop.run())
async def dependent():
    inner = await worker("inner", 2)
    return "dep(" + inner + ")"
loop2 = Loop()
loop2.spawn(dependent())
loop2.spawn(worker("side", 1))
print("ticks2", loop2.run())
async def gather_all():
    results = []
    for i in range(3):
        results.append(await worker(f"g{i}", i))
    return results
loop3 = Loop()
loop3.spawn(gather_all())
print("ticks3", loop3.run())

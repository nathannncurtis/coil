class Loop:
    def __init__(self):
        self.ready = []
    def spawn(self, c):
        self.ready.append(c)
    def run(self):
        while self.ready:
            c = self.ready.pop(0)
            try:
                c.send(None)
            except StopIteration:
                continue
            self.ready.append(c)
class Tick:
    def __await__(self):
        yield "tick"
        return 1
async def ticker(n):
    total = 0
    for _ in range(n):
        total += await Tick()
    return total
async def consume(agen):
    out = []
    while True:
        try:
            v = await agen.asend(None)
            out.append(v)
        except StopAsyncIteration:
            return out
async def numbers(limit):
    for i in range(limit):
        await Tick()
        yield i * 10
loop = Loop()
loop.spawn(consume(numbers(3)))
loop.run()
print("driven")
result = []
async def main():
    r = await ticker(3)
    vals = await consume(numbers(4))
    return (r, vals)
m = main()
while True:
    try:
        m.send(None)
    except StopIteration as e:
        print(e.value)
        break
async def failing_agen():
    yield 1
    raise ValueError("ag-fail")
async def drive_fail():
    ag = failing_agen()
    try:
        while True:
            await ag.asend(None)
    except ValueError as e:
        return "caught " + str(e)
df = drive_fail()
while True:
    try:
        df.send(None)
    except StopIteration as e:
        print(e.value)
        break

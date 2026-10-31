def nums(n):
    for i in range(n):
        yield i
def evens(src):
    for x in src:
        if x % 2 == 0:
            yield x
def squares(src):
    for x in src:
        yield x * x
def take(src, n):
    count = 0
    for x in src:
        if count >= n:
            return
        count += 1
        yield x
print(list(take(squares(evens(nums(20))), 5)))
def pairs(src):
    it = iter(src)
    for a in it:
        yield (a, next(it))
print(list(pairs([1, 2, 3, 4])))
try:
    print(list(pairs([1, 2, 3, 4, 5])))
except RuntimeError as e:
    print("RuntimeError", e)
def running(src):
    total = 0
    for x in src:
        total += x
        yield total
print(list(running(nums(5))))
def unique(src):
    seen = set()
    for x in src:
        if x not in seen:
            seen.add(x)
            yield x
print(list(unique([1, 2, 1, 3, 2])))
def chunk_pipe(xs, n):
    return (xs[i:i + n] for i in range(0, len(xs), n))
print(list(chunk_pipe(list(range(7)), 3)))

def make_fact():
    def fact(n):
        return 1 if n <= 1 else n * fact(n - 1)
    return fact
print(make_fact()(10))
def even_odd():
    def is_even(n):
        return True if n == 0 else not is_odd(n - 1)
    def is_odd(n):
        return False if n == 0 else not is_even(n - 1)
    return is_even, is_odd
e, o = even_odd()
print(e(10), o(10), e(7))
def ackermann():
    def ack(m, n):
        if m == 0:
            return n + 1
        if n == 0:
            return ack(m - 1, 1)
        return ack(m - 1, ack(m, n - 1))
    return ack
print(ackermann()(2, 3))
def deep_list():
    def build(d):
        return [] if d == 0 else [build(d - 1)]
    def depth(v):
        return 0 if not v else 1 + depth(v[0])
    return build, depth
build, depth = deep_list()
print(depth(build(50)))
def collatz_steps():
    steps = {}
    def go(n):
        if n == 1:
            return 0
        if n in steps:
            return steps[n]
        r = 1 + go(n // 2 if n % 2 == 0 else 3 * n + 1)
        steps[n] = r
        return r
    return go
go = collatz_steps()
print([go(n) for n in (1, 6, 7, 27)])
def hanoi():
    moves = []
    def solve(n, a, b, c):
        if n:
            solve(n - 1, a, c, b)
            moves.append((a, c))
            solve(n - 1, b, a, c)
    return solve, moves
solve, moves = hanoi()
solve(3, "A", "B", "C")
print(len(moves), moves[:3], moves[-2:])

def fact(n):
    return 1 if n <= 1 else n * fact(n - 1)
print(fact(10))
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)
print(fib(15))
def ack(m, n):
    if m == 0:
        return n + 1
    if n == 0:
        return ack(m - 1, 1)
    return ack(m - 1, ack(m, n - 1))
print(ack(2, 3))
def sumtree(t):
    if isinstance(t, list):
        return sum(sumtree(x) for x in t)
    return t
print(sumtree([1, [2, [3, 4], 5], 6]))
def mutual_a(n):
    return 0 if n == 0 else mutual_b(n - 1)
def mutual_b(n):
    return mutual_a(n) if n else 1
print(mutual_a(5), mutual_b(4))
def deep(n):
    return 0 if n == 0 else 1 + deep(n - 1)
print(deep(200))

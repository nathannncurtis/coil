def gcd(a, b):
    while b:
        a, b = b, a % b
    return a
print(gcd(48, 18), gcd(17, 5))
def sieve(n):
    flags = [True] * (n + 1)
    flags[0] = flags[1] = False
    for i in range(2, int(n ** 0.5) + 1):
        if flags[i]:
            for j in range(i * i, n + 1, i):
                flags[j] = False
    return [i for i, f in enumerate(flags) if f]
print(sieve(30))
def collatz(n):
    seq = [n]
    while n != 1:
        n = n // 2 if n % 2 == 0 else 3 * n + 1
        seq.append(n)
    return seq
print(collatz(6))
def bsearch(xs, t):
    lo, hi = 0, len(xs) - 1
    while lo <= hi:
        mid = (lo + hi) // 2
        if xs[mid] == t:
            return mid
        if xs[mid] < t:
            lo = mid + 1
        else:
            hi = mid - 1
    return -1
print(bsearch([1, 3, 5, 7, 9], 7), bsearch([1, 3], 4))
def fib_iter(n):
    a, b = 0, 1
    for _ in range(n):
        a, b = b, a + b
    return a
print([fib_iter(i) for i in range(10)])
def pascal(n):
    row = [1]
    out = [row]
    for _ in range(n):
        row = [1] + [row[i] + row[i + 1] for i in range(len(row) - 1)] + [1]
        out.append(row)
    return out
print(pascal(4))
def digits_sum(n):
    return sum(int(d) for d in str(abs(n)))
print(digits_sum(9875))

print(list(range(5)), list(range(2, 5)), list(range(0, 10, 3)))
print(list(range(5, 0, -1)), list(range(0)), list(range(5, 5)))
print(len(range(10)), len(range(0, 10, 3)), len(range(10, 0, -2)))
print(3 in range(5), 7 in range(5), 3 not in range(4, 8))
print(range(3)[1], range(3)[-1], range(0, 10, 3)[2])
print(range(1, 5), range(0, 10, 2))
print(sum(range(100)), max(range(3)), min(range(3)))
print(list(reversed(range(3))), list(reversed(range(1, 6, 2))))
total = 1
for i in range(1, 6):
    total *= i
print(total)
n = 0
for _ in range(3):
    n += 1
print(n)
print(list(range(-3, 3)))
print([i * i for i in range(-2, 3)])
print(list(range(10))[:3], list(range(10))[::4])
print(range(3) == range(3) if False else "skip")
big = range(10 ** 6)
print(len(big), big[999999])

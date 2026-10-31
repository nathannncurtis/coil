def find_first_divisible(xs, k):
    i = 0
    while i < len(xs):
        if xs[i] % k == 0:
            break
        i += 1
    else:
        return "none"
    return (i, xs[i])
print(find_first_divisible([3, 7, 8, 5], 4))
print(find_first_divisible([3, 7, 5], 4))
def classify(n):
    while n > 1:
        n = n // 2 if n % 2 == 0 else 3 * n + 1
    else:
        return "reached-1"
print(classify(6), classify(7))
found = False
for x in [1, 3, 5]:
    if x % 2 == 0:
        found = True
        break
else:
    print("no-even")
print(found)
while True:
    try:
        raise ValueError
    except ValueError:
        print("inner-handled")
        break
n = 0
total = 0
while (n := n + 1) <= 3:
    if n == 2:
        continue
    total += n
else:
    total += 100
print(total)
while False:
    print("never")
else:
    print("else-only")
i = 3
while i:
    i -= 1
print(i)

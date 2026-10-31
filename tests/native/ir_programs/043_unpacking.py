a, b = 1, 2
print(a, b)
a, b = b, a
print(a, b)
x, *y = [1, 2, 3]
print(x, y)
*p, q = "abc"
print(p, q)
m, *mid, n = [1, 2, 3, 4, 5]
print(m, mid, n)
a = b = c = 0
a += 1
print(a, b, c)
x = y = [0]
x.append(1)
print(y)
u, (v, w) = 1, (2, 3)
print(u, v, w)
try:
    a, b, *c = [1]
except ValueError as e:
    print("ValueError", e)
try:
    a, b = [1, 2, 3]
except ValueError as e:
    print("ValueError", e)
try:
    a, b = [1]
except ValueError as e:
    print("ValueError", e)

m = [[1, 2], [3, 4]]
print(m, m[1][0])
m[0].append(5)
print(m)
flat = [x for row in m for x in row]
print(flat)
t = [i ** 2 for i in range(6)]
print(t)
even = [i for i in range(10) if i % 2 == 0]
print(even)
pairs = [(a, b) for a in range(2) for b in range(2)]
print(pairs)
z = [[y * 3 for y in range(x)] for x in range(4)]
print(z)
y = [1, [2, [3, [4]]]]
print(y[1][1][1][0], len(y))
a = [0, 0]
b = [a, a]
b[0][0] = 9
print(b, a)
print(list(range(3)) == [0, 1, 2])

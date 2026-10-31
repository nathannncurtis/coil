s = {5, 1, 3, 2, 4}
print(s)
t = set()
for i in range(8):
    t.add(i * 10)
print(t)
print(sorted(t | {5}))
u = {1}
u |= {2, 3}
u &= {2, 3, 4}
print(u, len(u))
print(list({0}))

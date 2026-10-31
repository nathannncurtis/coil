x = 10
x += 5
x -= 3
x *= 2
x //= 3
x %= 5
x **= 2
print(x)
s = "a"
s += "b"
print(s)
xs = [1]
xs += [2]
xs *= 2
print(xs)
d = {}
d["k"] = 1
d["k"] += 10
print(d)
class C:
    pass
c = C()
c.v = 5
c.v *= 3
c.v -= 1
print(c.v)
t = (1, [2])
t[1].append(3)
print(t)
b = 0b1010
b |= 0b0110
b &= 0b1110
b ^= 1
b <<= 2
b >>= 1
print(b)

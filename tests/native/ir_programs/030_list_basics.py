xs = [1, 2, 3]
print(xs, len(xs), xs[0], xs[-1])
xs.append(4)
xs.extend([5, 6])
xs.insert(0, 0)
print(xs)
print(xs.pop(), xs.pop(0), xs)
xs.remove(2)
print(xs)
print(xs.index(4), xs.count(3))
xs.reverse()
print(xs)
ys = [3, 1, 2]
ys.sort()
print(ys)
ys.sort(reverse=True)
print(ys)
zs = [10, 2, 33, 4]
print(sorted(zs), sorted(zs, reverse=True))
zs.sort(key=lambda v: -v)
print(zs)
print(zs[1:3], zs[:2], zs[-2:], zs[::2], zs[::-1])
del zs[0]
print(zs)
zs[0:2] = [7, 8]
print(zs)
print([1, 2] + [3], [0] * 3, 2 in zs, 99 not in zs)
try:
    zs[99]
except IndexError as e:
    print("IndexError", e)
try:
    zs.remove(12345)
except ValueError as e:
    print("ValueError", e)

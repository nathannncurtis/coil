xs = list(range(10))
print(xs[2:7], xs[::3], xs[::-2])
print(xs[8:2:-1], xs[-3:], xs[:-3])
s = slice(1, 5)
print(xs[s], s.start, s.stop, s.step)
print(slice(None).start)
print(slice(0, 10, 2).indices(10))
ys = [0] * 5
ys[1:3] = [8, 9]
print(ys)
zs = list(range(5))
del zs[1:3]
print(zs)
print("abcdef"[1:4:2], (1, 2, 3, 4)[::-1])
print(len(xs[3:100]), xs[100:105])

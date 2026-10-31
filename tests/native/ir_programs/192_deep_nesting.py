def level1(a):
    x = a
    def level2(b):
        y = x + b
        def level3(c):
            z = y + c
            def level4(d):
                return z + d + x
            return level4
        return level3
    return level2
print(level1(1)(2)(3)(4))
def make_adders_nested():
    def outer_adder(n):
        def inner_adder(m):
            def deepest(k):
                return n + m + k
            return deepest
        return inner_adder
    return outer_adder
print(make_adders_nested()(10)(20)(30))
cond = 3
r = "a" if cond == 1 else "b" if cond == 2 else "c" if cond == 3 else "d"
print(r)
print((((1 + 2) * 3) - 4) // 2)
print([[[5]][0]][0][0])
xs = (1, (2, (3, (4,))))
print(xs[1][1][1][0])
def deep_return(x):
    if x > 10:
        return "big"
    if x > 5:
        return "mid"
    return deep_return(x * 2)
print(deep_return(3), deep_return(6), deep_return(12))
print([(a, (b, c)) for a in [1] for b in [2] for c in [3]])
print({k: {k2: v2 for k2, v2 in [("i", 1)]} for k in ["o"]})

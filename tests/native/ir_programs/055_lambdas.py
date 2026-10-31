sq = lambda x: x * x
print(sq(9))
add = lambda a, b=2: a + b
print(add(1), add(1, 5))
print((lambda *a: a)(1, 2))
print(sorted(["bbb", "a", "cc"], key=len))
print(sorted([3, -1, 2], key=lambda v: abs(v)))
print(max([1, 5, 3], key=lambda v: -v))
print(list(map(lambda x: x + 1, [1, 2])))
print(list(filter(lambda x: x > 1, [0, 1, 2, 3])))
fns = [lambda: i for i in range(3)]
print([f() for f in fns])
fns2 = [lambda i=i: i for i in range(3)]
print([f() for f in fns2])
print((lambda x: (lambda y: x + y))(1)(2))
compose = lambda f, g: lambda x: f(g(x))
print(compose(lambda v: v * 2, lambda v: v + 1)(5))
